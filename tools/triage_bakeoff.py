#!/usr/bin/env python3
"""Bake-off decision-моделей (Ollama /v1/systemone) на корпусе триажа фактов.

Честная оценка ДО внедрения: 1078 лейбленных кейсов (facts.triage.json),
каждая модель отвечает на policy-вопрос «мусор ли этот факт» одним
forward-pass'ом → метрики против живых лейблов + матрица согласия с
logreg-головой + потенциал ансамбля (dual-agreement).

NB: logreg-голова обучена НА ЭТОМ ЖЕ корпусе — её скоринги in-sample
(честные OOF-числа: acc 0.785 / F1 0.875 из triage_train train).
Decision-модели оцениваются честным zero-shot.

Запуск:
  triage_bakeoff.py run [model ...]   — скоринг (по умолчанию все кандидаты)
  triage_bakeoff.py report            — метрики + согласие из bakeoff_results.json
  triage_bakeoff.py logreg            — скоринг logreg-головой (subprocess rank)
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

WIKI = Path(os.environ.get("UWUWU_WIKI", str(Path.home() / ".uwuwu")))
CORPUS = WIKI / "facts.triage.json"
RESULTS = Path(__file__).resolve().parent / "bakeoff_results.json"
VENV_PY = Path(__file__).resolve().parent / ".venv-triage/bin/python"
OLLAMA = os.environ.get("OLLAMA_HOST_URL", "http://localhost:11434")

MODELS = ["laya:322m-multilingual", "d3:2b", "d1:3b", "tev1:4b"]

# Policy-вопрос: формулировка из практики dreaming-триажа (критерии сжаты —
# у laya бюджет 512 токен на instructions+criteria+state вместе).
QUESTION = {
    "garbage": {
        "type": "noul",
        "instructions": (
            "Statement comes from an engineering session log. Is it DISPOSABLE "
            "process noise (true) or KEEPWORTHY knowledge (false)?"
        ),
        "criteria": {
            "true": (
                "Disposable noise: CI/PR/commit status reports, test counts, "
                "pipeline green/red updates, bare metrics counters, duplicates, "
                "plans without implementation, routine bookkeeping, meta-facts "
                "about the logging process itself."
            ),
            "false": (
                "Keepworthy: root cause with fix, non-obvious gotcha or trap, "
                "working recipe or technique, product decision with rationale, "
                "new capability or feature, infrastructure finding with evidence."
            ),
        },
    }
}


def systemone(model: str, state: str, retries: int = 3) -> float:
    """p(garbage) одним forward-pass'ом; keep_alive держит модель в памяти."""
    payload = json.dumps(
        {
            "model": model,
            "state": state,
            "questions": QUESTION,
            "keep_alive": "30m",
        }
    ).encode()
    req = urllib.request.Request(
        f"{OLLAMA}/v1/systemone",
        data=payload,
        headers={"Content-Type": "application/json"},
    )
    for attempt in range(retries):
        try:
            with urllib.request.urlopen(req, timeout=180) as r:
                body = json.loads(r.read().decode())
            return float(body["answers"]["garbage"]["noul"])
        except (urllib.error.URLError, KeyError, ValueError, json.JSONDecodeError) as e:
            if attempt == retries - 1:
                print(
                    f"  !! {model}: запрос провален после {retries} попыток: {e}",
                    file=sys.stderr,
                )
                return -1.0
            time.sleep(3 * (attempt + 1))
    return -1.0


def load_results() -> dict:
    return json.loads(RESULTS.read_text(encoding="utf-8")) if RESULTS.exists() else {}


def cmd_run(args: argparse.Namespace) -> int:
    models = args.models or MODELS
    corpus = json.loads(CORPUS.read_text(encoding="utf-8"))
    results = load_results()
    for model in models:
        done = results.get(model, {})
        todo = [(sha, c) for sha, c in corpus.items() if sha not in done]
        print(
            f"[{model}] корпуc {len(corpus)}, пройдено {len(done)}, осталось {len(todo)}",
            flush=True,
        )
        t0 = time.time()
        for i, (sha, case) in enumerate(todo, 1):
            p = systemone(model, case["statement"])
            if p >= 0:
                done[sha] = p
            if i % 50 == 0 or i == len(todo):
                results[model] = done
                RESULTS.write_text(json.dumps(results), encoding="utf-8")
                rate = i / (time.time() - t0)
                print(f"  {i}/{len(todo)} ({rate:.1f} кейс/с)", flush=True)
        results[model] = done
        RESULTS.write_text(json.dumps(results), encoding="utf-8")
    return 0


def cmd_logreg(_: argparse.Namespace) -> int:
    """Скоринг logreg-головой через rank (кеш эмбеддингов корпуса уже тёплый)."""
    import subprocess
    import tempfile

    corpus = json.loads(CORPUS.read_text(encoding="utf-8"))
    results = load_results()
    with tempfile.TemporaryDirectory(prefix="bakeoff-logreg-") as td:
        files = {}
        for sha, case in corpus.items():
            f = Path(td) / f"{sha}.md"
            f.write_text(
                f"---\nsha: {sha}\n---\n{case['statement']}\n", encoding="utf-8"
            )
            files[f.name[:-3]] = sha
        proc = subprocess.run(
            [
                str(VENV_PY),
                str(Path(__file__).resolve().parent / "triage_train.py"),
                "rank",
                str(Path(td) / "*.md"),
            ],
            capture_output=True,
            text=True,
            timeout=3600,
            check=False,
        )
        scores = {}
        for line in proc.stdout.splitlines():
            parts = line.split()
            if len(parts) >= 2 and parts[0].replace(".", "").isdigit():
                sha = files.get(Path(parts[1]).stem)
                if sha:
                    scores[sha] = float(parts[0])
        if len(scores) < len(corpus) * 0.9:
            print(
                f"!! logreg: распознано {len(scores)}/{len(corpus)} строк вывода",
                file=sys.stderr,
            )
            print(proc.stderr[-500:], file=sys.stderr)
            return 2
        results["logreg"] = scores
        RESULTS.write_text(json.dumps(results), encoding="utf-8")
        print(f"logreg: {len(scores)} кейсов")
    return 0


def auc(scores_pos: list[float], scores_neg: list[float]) -> float:
    """AUC через Mann-Whitney U (без sklearn)."""
    if not scores_pos or not scores_neg:
        return float("nan")
    wins = ties = 0
    for p in scores_pos:
        for n in scores_neg:
            if p > n:
                wins += 1
            elif p == n:
                ties += 1
    return (wins + 0.5 * ties) / (len(scores_pos) * len(scores_neg))


def metrics(pairs: list[tuple[float, str]], thr: float) -> dict:
    tp = fp = tn = fn = 0
    for p, label in pairs:
        pred_garbage = p >= thr
        if pred_garbage and label == "garbage":
            tp += 1
        elif pred_garbage:
            fp += 1
        elif label == "garbage":
            fn += 1
        else:
            tn += 1
    prec = tp / (tp + fp) if tp + fp else 0.0
    rec = tp / (tp + fn) if tp + fn else 0.0
    f1 = 2 * prec * rec / (prec + rec) if prec + rec else 0.0
    return {
        "threshold": round(thr, 3),
        "accuracy": round((tp + tn) / len(pairs), 3),
        "precision_garbage": round(prec, 3),
        "recall_garbage": round(rec, 3),
        "f1_garbage": round(f1, 3),
        "confusion": {"tp": tp, "fp": fp, "fn": fn, "tn": tn},
    }


def best_f1_threshold(pairs: list[tuple[float, str]]) -> tuple[float, dict]:
    best = (0.0, None)
    for thr in (i / 100 for i in range(5, 100, 5)):
        m = metrics(pairs, thr)
        if m["f1_garbage"] > best[0]:
            best = (m["f1_garbage"], m)
    return (best[1]["threshold"], best[1]) if best[1] else (0.5, metrics(pairs, 0.5))


def cmd_report(_: argparse.Namespace) -> int:
    corpus = json.loads(CORPUS.read_text(encoding="utf-8"))
    results = load_results()
    if not results:
        print("нет результатов — сначала run")
        return 2
    print(
        f"корпус: {len(corpus)} "
        f"(garbage {sum(c['label'] == 'garbage' for c in corpus.values())}, "
        f"keep {sum(c['label'] == 'keep' for c in corpus.values())})\n"
    )
    table = {}
    for model, scores in results.items():
        pairs = [
            (p, corpus[sha]["label"]) for sha, p in scores.items() if sha in corpus
        ]
        if len(pairs) < 10:
            print(f"{model}: мало данных ({len(pairs)})")
            continue
        pos = [p for p, l in pairs if l == "garbage"]
        neg = [p for p, l in pairs if l == "keep"]
        m05 = metrics(pairs, 0.5)
        thr_best, mbest = best_f1_threshold(pairs)
        table[model] = {
            "n": len(pairs),
            "auc": round(auc(pos, neg), 3),
            "at_0.5": m05,
            "best_f1": mbest,
        }
        print(f"═ {model} (n={len(pairs)})")
        print(
            f"  AUC {table[model]['auc']} | @0.5: acc {m05['accuracy']} "
            f"P {m05['precision_garbage']} R {m05['recall_garbage']} "
            f"F1 {m05['f1_garbage']} {m05['confusion']}"
        )
        print(
            f"  best-F1 @thr {thr_best}: acc {mbest['accuracy']} "
            f"P {mbest['precision_garbage']} R {mbest['recall_garbage']} "
            f"F1 {mbest['f1_garbage']}\n"
        )
    # ансамбль: dual-agreement с logreg (обе ≥ 0.5 → garbage-вердикт)
    if "logreg" in results and len(results) > 1:
        lr = results["logreg"]
        for model in results:
            if model == "logreg":
                continue
            both = [
                (sha, results[model][sha])
                for sha in results[model]
                if sha in lr and sha in corpus
            ]
            if len(both) < 10:
                continue
            dual = metrics(
                [(max(p, lr[sha]), corpus[sha]["label"]) for sha, p in both], 0.5
            )
            # dual-agreement: обе модели ≥0.5; max() как прокси (обе≥0.5 ⟺ max≥0.5)
            agree_garbage = sum(1 for sha, p in both if p >= 0.5 and lr[sha] >= 0.5)
            agree_keep = sum(1 for sha, p in both if p < 0.5 and lr[sha] < 0.5)
            print(f"═ logreg + {model}: dual-agreement (обе ≥0.5)")
            print(
                f"  согласных garbage-вердиктов: {agree_garbage}, keep: {agree_keep}, "
                f"расхождений: {len(both) - agree_garbage - agree_keep}"
            )
            print(
                f"  P {dual['precision_garbage']} R {dual['recall_garbage']} "
                f"F1 {dual['f1_garbage']} {dual['confusion']}\n"
            )
    return 0


def main() -> int:
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run", help="скоринг моделей по корпусу")
    r.add_argument("models", nargs="*")
    sub.add_parser("logreg", help="скоринг logreg-головой (бейзлайн)")
    sub.add_parser("report", help="метрики и согласие")
    args = p.parse_args()
    return {"run": cmd_run, "logreg": cmd_logreg, "report": cmd_report}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
