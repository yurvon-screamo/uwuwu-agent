#!/usr/bin/env python3
"""triage_train — обучение triage-головы (garbage/keep) для фактов вики.

Архитектура (Simlicity First): логрегрессия на numpy поверх эмбеддингов
qwen3-embedding:0.6b (ollama /api/embed, batch, dim 1024) — тот же энкодер,
что в семпоиске вики. Никакого torch: корпус ~531 кейс, фичи 1024, full-batch.

Требования окружения: ollama с qwen3-embedding:0.6b; numpy
(uv venv tools/.venv-triage + uv pip install numpy — см. README tools).
Запуск: .venv-triage/bin/python triage_train.py train|infer|stats

Команды:
  train  — кеш эмбеддингов (npz, инвалидируется по sha корпуса) → 5-fold
           стратифицированная CV (seed) → OOF-метрики (acc/F1/P/R/AUC +
           порог max-F1) → финал на всём корпусе → model.npz + model.json
           в UWUWU_WIKI/triage_model/.
  infer  — вероятность garbage для --text (или stdin) по сохранённой модели.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import time
import urllib.request
from pathlib import Path

import numpy as np

WIKI = Path(os.environ.get("UWUWU_WIKI", str(Path.home() / ".uwuwu")))
CORPUS = WIKI / "facts.triage.json"
CACHE = Path(__file__).resolve().parent / "triage_embed_cache.npz"
MODEL_DIR = WIKI / "triage_model"
OLLAMA = os.environ.get("OLLAMA_URL", "http://localhost:11434")
EMBED_MODEL = "qwen3-embedding:0.6b"
DIM = 1024
BATCH = 32
LABELS = ("garbage", "keep")
EPOCHS = 400
LR = 0.5
L2 = 1e-3


def embed_all(texts: list[str]) -> np.ndarray:
    """Эмбеддинги батчами через ollama /api/embed; retry с паузой."""
    out = np.zeros((len(texts), DIM), dtype=np.float32)
    for i in range(0, len(texts), BATCH):
        chunk = texts[i : i + BATCH]
        payload = json.dumps({"model": EMBED_MODEL, "input": chunk}).encode()
        for attempt in range(5):
            try:
                req = urllib.request.Request(
                    f"{OLLAMA}/api/embed",
                    data=payload,
                    headers={"Content-Type": "application/json"},
                )
                with urllib.request.urlopen(req, timeout=300) as r:
                    emb = json.loads(r.read())["embeddings"]
                if len(emb) != len(chunk):
                    raise RuntimeError(
                        f"ollama вернула {len(emb)} эмбеддингов на {len(chunk)} текстов"
                    )
                out[i : i + len(chunk)] = emb
                break
            except Exception as e:
                if attempt == 4:
                    raise
                print(f"  embed retry {attempt + 1}: {e}", file=sys.stderr)
                time.sleep(3 * (attempt + 1))
        if (i // BATCH) % 4 == 0:
            print(f"  embedded {i + len(chunk)}/{len(texts)}", flush=True)
    return out


def load_embeddings(texts: list[str], corpus_sha: str) -> np.ndarray:
    if CACHE.exists():
        c = np.load(CACHE, allow_pickle=False)
        if str(c["sha"]) == corpus_sha:
            print(f"embed cache hit ({len(texts)}x{DIM})")
            return c["emb"]
    print(f"embedding {len(texts)} statements via {EMBED_MODEL}…")
    emb = embed_all(texts)
    np.savez(CACHE, emb=emb, sha=np.array(corpus_sha))
    return emb


def two_stage_fit(
    x: np.ndarray, y: np.ndarray, pre: tuple[np.ndarray, np.ndarray] | None
) -> tuple[np.ndarray, np.ndarray]:
    """Одна точка двухстадийного рецепта: (претрейн на внешних) → fine-tune."""
    init = fit_logreg(*pre, epochs=200) if pre else None
    return fit_logreg(
        x,
        y,
        init=init,
        epochs=200 if init is not None else EPOCHS,
        lr=LR * 0.5 if init is not None else LR,
    )


def stratified_folds(y: np.ndarray, folds: int, seed: int) -> list[np.ndarray]:
    """Индексы фолда i = строки, попадающие в val-часть фолда i."""
    rng = np.random.default_rng(seed)
    val_idx: list[np.ndarray] = []
    for cls in (0, 1):
        idx = np.flatnonzero(y == cls)
        rng.shuffle(idx)
        val_idx.append(idx)
    out: list[np.ndarray] = []
    for f in range(folds):
        parts = [idx_split[f::folds] for idx_split in val_idx]
        out.append(np.concatenate(parts))
    return out


def fit_mlp(
    x: np.ndarray,
    y: np.ndarray,
    hidden: int = 256,
    epochs: int = 600,
    lr: float = 0.3,
    l2: float = 1e-3,
    seed: int = 7,
) -> dict[str, np.ndarray]:
    """MLP: hidden ReLU -> 2 класса, weighted CE, full-batch, He-init."""
    rng = np.random.default_rng(seed)
    n, d = x.shape
    x1 = np.hstack([x, np.ones((n, 1), dtype=np.float32)])
    w1 = (rng.standard_normal((d + 1, hidden)) * np.sqrt(2.0 / (d + 1))).astype(
        np.float32
    )
    b2 = np.zeros(2, dtype=np.float32)
    w2 = (rng.standard_normal((hidden, 2)) * np.sqrt(2.0 / hidden)).astype(np.float32)
    counts = np.bincount(y, minlength=2)
    weights = (n / counts).astype(np.float32)
    y_onehot = np.eye(2, dtype=np.float32)[y]
    sw = weights[y][:, None]
    sw_sum = float(sw.sum())
    for _ in range(epochs):
        h = np.maximum(x1 @ w1, 0)
        logits = h @ w2 + b2
        logits -= logits.max(axis=1, keepdims=True)
        p = np.exp(logits)
        p /= p.sum(axis=1, keepdims=True)
        g2 = ((p - y_onehot) * sw) / sw_sum  # [n,2]
        gw2 = h.T @ g2 + l2 * w2
        gb2 = g2.sum(axis=0)
        gh = (g2 @ w2.T) * (h > 0)  # [n,hidden]
        gw1 = x1.T @ gh + l2 * w1
        w2 -= lr * gw2
        b2 -= lr * gb2
        w1 -= lr * gw1
    return {"w1": w1, "w2": w2, "b2": b2}


def mlp_scores(model: dict[str, np.ndarray], x: np.ndarray) -> np.ndarray:
    n = x.shape[0]
    x1 = np.hstack([x, np.ones((n, 1), dtype=np.float32)])
    h = np.maximum(x1 @ model["w1"], 0)
    logits = h @ model["w2"] + model["b2"]
    logits -= logits.max(axis=1, keepdims=True)
    p = np.exp(logits)
    p /= p.sum(axis=1, keepdims=True)
    return p[:, 1]


def fit_logreg(
    x: np.ndarray,
    y: np.ndarray,
    epochs: int = EPOCHS,
    lr: float = LR,
    l2: float = L2,
    init: tuple[np.ndarray, np.ndarray] | None = None,
) -> tuple[np.ndarray, np.ndarray]:
    """Full-batch softmax-логрегрессия, класс-веса inverse-frequency."""
    n, d = x.shape
    x1 = np.hstack([x, np.ones((n, 1), dtype=np.float32)])
    w = np.zeros((d + 1, 2), dtype=np.float32)
    if init is not None:
        w[:-1] = init[0]
        w[-1] = init[1]
    counts = np.bincount(y, minlength=2)
    weights = (n / counts).astype(np.float32)  # inverse freq
    y_onehot = np.eye(2, dtype=np.float32)[y]
    sw = weights[y][:, None]
    sw_sum = float(sw.sum())
    for _ in range(epochs):
        logits = x1 @ w
        logits -= logits.max(axis=1, keepdims=True)
        p = np.exp(logits)
        p /= p.sum(axis=1, keepdims=True)
        grad = x1.T @ ((p - y_onehot) * sw) / sw_sum
        grad[1:] += l2 * w[1:]  # bias не регуляризуем
        w -= lr * grad
    return w[:-1], w[-1]


def auc_mann_whitney(scores: np.ndarray, y: np.ndarray) -> float:
    """AUC ранжированием (класс 1 = garbage положительный); midrank для связей."""
    pos, neg = scores[y == 1], scores[y == 0]
    if len(pos) == 0 or len(neg) == 0:
        return float("nan")
    allv = np.concatenate([pos, neg])
    order = np.argsort(allv, kind="stable")
    ranks = np.empty_like(order, dtype=np.float64)
    # связанные значения получают средний ранг (midrank)
    sorted_v = allv[order]
    i = 0
    while i < len(sorted_v):
        j = i
        while j + 1 < len(sorted_v) and sorted_v[j + 1] == sorted_v[i]:
            j += 1
        ranks[order[i : j + 1]] = (i + j + 2) / 2.0  # 1-based среднее
        i = j + 1
    r_pos = ranks[: len(pos)].sum()
    return float((r_pos - len(pos) * (len(pos) + 1) / 2) / (len(pos) * len(neg)))


def max_f1_threshold(scores: np.ndarray, y: np.ndarray) -> tuple[float, float]:
    best_t, best_f1 = 0.5, -1.0
    for t in np.unique(scores):
        pred = scores >= t
        tp = int(((pred == 1) & (y == 1)).sum())
        fp = int(((pred == 1) & (y == 0)).sum())
        fn = int(((pred == 0) & (y == 1)).sum())
        f1 = 2 * tp / (2 * tp + fp + fn) if tp else 0.0
        if f1 > best_f1:
            best_f1, best_t = f1, float(t)
    return best_t, best_f1


def metrics(pred: np.ndarray, y: np.ndarray) -> dict:
    tp = int(((pred == 1) & (y == 1)).sum())
    fp = int(((pred == 1) & (y == 0)).sum())
    fn = int(((pred == 0) & (y == 1)).sum())
    tn = int(((pred == 0) & (y == 0)).sum())
    prec = tp / (tp + fp) if tp + fp else 0.0
    rec = tp / (tp + fn) if tp + fn else 0.0
    return {
        "accuracy": round((tp + tn) / len(y), 3),
        "f1_garbage": round(2 * tp / (2 * tp + fp + fn), 3) if tp else 0.0,
        "precision_garbage": round(prec, 3),
        "recall_garbage": round(rec, 3),
        "confusion": {"tp": tp, "fp": fp, "fn": fn, "tn": tn},
    }


def cmd_train(args: argparse.Namespace) -> int:
    corpus = json.loads(CORPUS.read_text(encoding="utf-8"))
    # Guard вырожденных корпусов: внятная ошибка вместо трейсбека из глубины
    if len(corpus) < 2 * args.folds:
        print(
            f"корпус слишком мал: {len(corpus)} кейсов < 2×folds "
            f"({2 * args.folds}); нужен минимум по {args.folds} на класс",
            file=sys.stderr,
        )
        return 2
    texts = [v["statement"] for v in corpus.values()]
    y = np.array([0 if v["label"] == "keep" else 1 for v in corpus.values()])
    counts = np.bincount(y, minlength=2)
    if (counts == 0).any():
        print(
            f"в корпусе один класс: garbage={counts[1]} keep={counts[0]}; "
            "для обучения нужны оба",
            file=sys.stderr,
        )
        return 2
    days = sorted({v["day"] for v in corpus.values()})
    print(
        f"corpus: {len(texts)} (garbage={int(y.sum())}, keep={int((y == 0).sum())}), "
        f"days {days[0]}..{days[-1]}"
    )

    sha = hashlib.sha256("\x1e".join(texts).encode()).hexdigest()[:16]
    emb = load_embeddings(texts, sha)
    # L2-нормировка строк (qwen-эмбеддинги почти нормированы, но гарантия)
    emb = emb / (np.linalg.norm(emb, axis=1, keepdims=True) + 1e-9)
    assert np.isfinite(emb).all(), "non-finite embeddings — см. vllm-pooling bf16-урок"

    folds = stratified_folds(y, args.folds, args.seed)
    pre = None
    if args.pretrain:
        p = np.load(args.pretrain)
        try:
            px, py = p["x"], p["y"]
        except KeyError as e:
            print(f"pretrain npz без ключа {e} (нужны x/y)", file=sys.stderr)
            return 2
        if px.ndim != 2 or px.shape[1] != DIM or set(np.unique(py)) - {0, 1}:
            print(
                f"pretrain npz несовместим: x={px.shape}, y={np.unique(py)}",
                file=sys.stderr,
            )
            return 2
        # контракт: внешние эмбеды L2-нормируем здесь — их источник не гарантирован
        px = px / (np.linalg.norm(px, axis=1, keepdims=True) + 1e-9)
        pre = (px, py)
        print(f"pretrain: {len(py)} внешних кейсов из {args.pretrain}")
    oof = np.zeros(len(y), dtype=np.float32)
    for f, val in enumerate(folds):
        train = np.setdiff1d(np.arange(len(y)), val)
        if args.arch == "mlp":
            model = fit_mlp(emb[train], y[train], seed=args.seed + f)
            oof[val] = mlp_scores(model, emb[val])
        else:
            w, b = two_stage_fit(emb[train], y[train], pre)
            logits = emb[val] @ w + b
            logits -= logits.max(axis=1, keepdims=True)
            p = np.exp(logits)
            p /= p.sum(axis=1, keepdims=True)
            oof[val] = p[:, 1]
        m = metrics((oof[val] >= 0.5).astype(int), y[val])
        print(
            f"fold {f + 1}: acc={m['accuracy']} f1={m['f1_garbage']} "
            f"(train {len(train)}/val {len(val)})"
        )

    thr, _ = max_f1_threshold(oof, y)
    # NB: порог подобран по полному OOF и применяется к тем же OOF-строкам —
    # метрики НА пороге слегка оптимистичны (AUC от порога не зависит — чист).
    m_thr = metrics((oof >= thr).astype(int), y)
    m_05 = metrics((oof >= 0.5).astype(int), y)
    auc = auc_mann_whitney(oof, y)
    majority = max(y.mean(), 1 - y.mean())
    print("\n=== OOF (5-fold, порог max-F1) ===")
    print(json.dumps({**m_thr, "auc": round(auc, 3)}, ensure_ascii=False))
    print(f"порог 0.5: {json.dumps(m_05)}")
    print(f"бейзлайн majority(garbage): {majority:.3f}")

    MODEL_DIR.mkdir(parents=True, exist_ok=True)
    if args.arch == "mlp":
        np.savez(MODEL_DIR / "model.npz", **fit_mlp(emb, y, seed=args.seed))
    else:
        w, b = two_stage_fit(emb, y, pre)
        np.savez(MODEL_DIR / "model.npz", w=w, b=b)
    meta = {
        "model": f"{args.arch}-l2-normemb",
        "encoder": EMBED_MODEL,
        "dim": DIM,
        "trained": time.strftime("%Y-%m-%d"),
        "corpus": len(texts),
        "corpus_sha": sha,
        "days": days,
        "seed": args.seed,
        "threshold": round(thr, 4),
        "oof": {**m_thr, "auc": round(auc, 3)},
        "oof_thr05": m_05,
        "epochs": EPOCHS,
        "lr": LR,
        "l2": L2,
    }
    (MODEL_DIR / "model.json").write_text(
        json.dumps(meta, ensure_ascii=False, indent=1), encoding="utf-8"
    )
    print(
        f"\nмодель: {MODEL_DIR}/model.npz (+model.json); порог {thr:.3f}, "
        f"OOF F1(garbage) {m_thr['f1_garbage']}"
    )
    return 0


def cmd_infer(args: argparse.Namespace) -> int:
    m = np.load(MODEL_DIR / "model.npz")
    meta = json.loads((MODEL_DIR / "model.json").read_text(encoding="utf-8"))
    text = args.text if args.text else sys.stdin.read().strip()
    if not text:
        print("пустой текст", file=sys.stderr)
        return 2
    vec = embed_all([text])[0]
    vec = vec / (np.linalg.norm(vec) + 1e-9)
    npz_arch = "mlp" if "w1" in m else "linear"
    if meta.get("model", "").split("-")[0] != npz_arch:
        print(
            f"рассинхрон model.json ({meta.get('model')}) и model.npz "
            f"({npz_arch}) — переобучи модель (train)",
            file=sys.stderr,
        )
        return 2
    if "w1" in m:
        p_garbage = float(
            mlp_scores({"w1": m["w1"], "w2": m["w2"], "b2": m["b2"]}, vec[None, :])[0]
        )
    else:
        logits = vec @ m["w"] + m["b"]
        logits = logits - logits.max()  # стабилизация как в train
        p_garbage = float(np.exp(logits[1]) / np.exp(logits).sum())
    verdict = "garbage" if p_garbage >= meta["threshold"] else "keep"
    print(
        json.dumps(
            {
                "p_garbage": round(p_garbage, 4),
                "threshold": meta["threshold"],
                "verdict": verdict,
                "encoder": meta["encoder"],
            },
            ensure_ascii=False,
        )
    )
    return 0


def cmd_rank(args: argparse.Namespace) -> int:
    """Скоринг фактов головой: список путей/глобов → таблица p_garbage по убыванию."""
    import glob as _glob

    from triage_log import statement_of

    m = np.load(MODEL_DIR / "model.npz")
    meta = json.loads((MODEL_DIR / "model.json").read_text(encoding="utf-8"))
    npz_arch = "mlp" if "w1" in m else "linear"
    if meta.get("model", "").split("-")[0] != npz_arch:
        print(
            f"рассинхрон model.json ({meta.get('model')}) и model.npz ({npz_arch}) "
            f"— переобучи модель (train)",
            file=sys.stderr,
        )
        return 2
    texts, files, skipped = [], [], 0
    for pat in args.files:
        # stdlib glob: поддерживает абсолютные паттерны (pathlib — нет)
        matches = _glob.glob(pat) if any(c in pat for c in "*?[") else [pat]
        for name in matches:
            path = Path(name)
            if not path.is_file():
                print(f"пропущен (не файл): {name}", file=sys.stderr)
                skipped += 1
                continue
            stmt = statement_of(path.read_text(encoding="utf-8"))
            if stmt:
                texts.append(stmt)
                files.append(str(path))
            else:
                skipped += 1
    if not texts:
        print("нет валидных файлов фактов", file=sys.stderr)
        return 2
    emb = embed_all(texts)
    emb = emb / (np.linalg.norm(emb, axis=1, keepdims=True) + 1e-9)
    if "w1" in m:
        scores = mlp_scores({"w1": m["w1"], "w2": m["w2"], "b2": m["b2"]}, emb)
    else:
        logits = np.hstack([emb, np.ones((len(emb), 1), dtype=np.float32)]) @ np.vstack(
            [m["w"], m["b"][None, :]]
        )
        logits -= logits.max(axis=1, keepdims=True)
        p = np.exp(logits)
        p /= p.sum(axis=1, keepdims=True)
        scores = p[:, 1]
    order = np.argsort(-scores)
    thr = meta["threshold"]
    hi = 0
    for i in order:
        mark = " *" if scores[i] >= 0.5 else ""
        hi += scores[i] >= 0.5
        print(f"{scores[i]:.3f}  {files[i]}{mark}")
    print(
        f"\nитого: {len(texts)} (пропущено невалидных: {skipped}), "
        f"кандидаты мусора (p>=0.5): {hi} "
        f"[точность ~{meta['oof_thr05']['precision_garbage']:.2f} по OOF]; "
        f"порог модели {thr:.3f} (max-F1, агрессивный)"
    )
    return 0


def main() -> int:
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = p.add_subparsers(dest="cmd", required=True)
    t = sub.add_parser("train", help="обучить на facts.triage.json")
    t.add_argument("--seed", type=int, default=7)
    t.add_argument("--folds", type=int, default=5)
    t.add_argument("--arch", choices=("mlp", "linear"), default="linear")
    t.add_argument("--pretrain", help="npz с внешним корпусом (x=эмбеддинги, y=0/1)")
    i = sub.add_parser("infer", help="вердикт для текста")
    i.add_argument("--text")
    r = sub.add_parser("rank", help="скоринг файлов фактов (пути/глобы)")
    r.add_argument("files", nargs="+")
    args = p.parse_args()
    if args.cmd == "train":
        return cmd_train(args)
    if args.cmd == "rank":
        return cmd_rank(args)
    return cmd_infer(args)


if __name__ == "__main__":
    sys.exit(main())
