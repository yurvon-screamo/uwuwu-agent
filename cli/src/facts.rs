//! facts — единый движок фактизации (экстракция → санитайзинг → полный дедуп
//! → запись факта → scoped git-коммит). Python (facts_extract/facts_migrate) —
//! оркестратор: вызывает `uwuwu-cli facts --stdin`, трактует JSON, ведёт
//! sidecar-атомарность сессий. Пайплайн существует в одном экземпляре (здесь).
//!
//! Мотивация порта (review): единая точка правды пайплайна + ручной режим без
//! python-окружения. Производительность — не мотивация (LLM-доминировано).

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde::Deserialize;
use sha1::{Digest as _, Sha1};
use unicode_normalization::UnicodeNormalization;

use crate::affine_client::AffineConfig;

use crate::affine_store::FactEntry;
use crate::wiki_root;

const OLLAMA_URL: &str = "http://localhost:11434";
const MODEL: &str = "qwen3.5:4b";
const TEMP: f32 = 0.0;
const SEED: u32 = 42;
const NUM_PREDICT: u32 = 2048;
const CALL_TIMEOUT_SECS: u64 = 300;

const JACCARD_THRESHOLD: f32 = 0.4;
const VERDICT_BATCH: usize = 40;
const MAX_FACTS_PER_CALL: usize = 12;
const MIN_DIGEST_CHARS: usize = 1000;
const STMT_MIN_CHARS: usize = 15;
const STMT_MAX_CHARS: usize = 600;
const TITLE_MAX_CHARS: usize = 120;
const TAG_MAX_CHARS: usize = 24;
const MAX_TAGS: usize = 4;
const DESC_MAX_CHARS: usize = 150;

const LOCK_PATH: &str = "facts.lock";
const LOCK_STALE_SECS: u64 = 300;
const LOCK_TOUCH_SECS: u64 = 30;

/// Промпт экстракции — verbatim-перенос из facts_extract.py (review-контракт:
/// побайтовая идентичность; любое изменение — отдельным сознательным шагом).
const EXTRACT_SYSTEM: &str = "\
Ты извлекаешь устойчивые факты из дайджеста рабочей сессии AI-агента.\n\
Факт = одно короткое утверждение, которое останется верным спустя месяцы и пригодится в будущей работе.\n\
\n\
ИЗВЛЕКАЙ: принятые решения и их причины; топологию/конфигурацию систем (стенды, сервисы, порты, версии, маршруты); проверенные грабли и их лечение; числовые факты (лимиты, метрики, объёмы, тайминги); долгоживущие состояния систем с датой.\n\
\n\
ПРОПУСКАЙ: метаданные САМОЙ сессии (её дату, id, рабочую директорию, агента, \"сессия проведена/успешно выполнена\"); probe-проверки и технические подтверждения (\"вернул PROBE-OK\", \"запрос прошёл\"); разовые сетевые ошибки; диалоговые мелочи; перефразировки уже извлечённого — один факт один раз.\n\
\n\
Формат: JSONL, по одному объекту на строку, БЕЗ другого текста:\n\
{\"statement\": \"русское утверждение 1-2 предложения\", \"title\": \"short english title\", \"tags\": [\"tag1\"], \"confidence\": 0.8}\n\
confidence 0.1-1.0 — насколько утверждение подтверждено дайджестом.";

const VERDICT_SYSTEM: &str = "\
Ты сравниваешь пары фактов из базы знаний. Для каждой пары вынеси вердикт:\n\
- \"duplicate\" — утверждения сообщают одно и то же (перефраз)\n\
- \"conflict\" — утверждения противоречат друг другу (X vs не-X)\n\
- \"distinct\" — разные факты (в т.ч. дополняют друг друга деталями)\n\
Формат: JSONL, по объекту на строку: {\"pair\": <номер>, \"verdict\": \"duplicate|conflict|distinct\"}\n\
Верни вердикт для КАЖДОЙ пары. Если противоречие есть хоть в одной паре — оно важнее дубля.";

// ---------- публичный контракт ----------

#[derive(serde::Serialize)]
pub struct FactsOutcome {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bad: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trivial: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub new: Vec<NewFact>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub dup: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub conflict: Vec<ConflictPair>,
}

#[derive(serde::Serialize)]
pub struct NewFact {
    pub slug: String,
    pub title: String,
}

#[derive(serde::Serialize)]
pub struct ConflictPair {
    pub slug: String,
    pub with: String,
}

/// Полный пайплайн фактизации произвольного текста.
/// `source` — метка источника (manual/<date> или opencode/<date>/<sid>).
/// `date` — день факта (месяц каталога + updated). stdout-JSON печатает вызывающий.
/// `stdin` — вход от оркестратора (дайджест сессии): тривиал-порог MIN_DIGEST_CHARS
/// активен; ручной аргумент-текст — намеренный ввод, порог не применяется.
pub async fn run_facts(text: &str, source: &str, date: &str, stdin: bool) -> FactsOutcome {
    match run_facts_inner(text, source, date, stdin).await {
        Ok((out, affine_facts)) => {
            // Publish runs OUTSIDE the pipeline lock: the hook does network I/O
            // (up to 120s per call) and must never hold the FS lock hostage
            // for concurrent facts runs waiting on their 60s deadline.
            if std::env::var("UWUWU_AFFINE_PUBLISH").ok().as_deref() == Some("1")
                && !affine_facts.is_empty()
            {
                publish_to_affine(&wiki_root(), &affine_facts).await;
            }
            out
        }
        Err(e) if e.to_string().contains("OLLAMA_UNREACHABLE") => FactsOutcome {
            ok: false,
            error: Some("ollama_unreachable".into()),
            bad: None,
            trivial: None,
            new: vec![],
            dup: vec![],
            conflict: vec![],
        },
        Err(e) => FactsOutcome {
            ok: false,
            error: Some(format!("{e:#}")),
            bad: None,
            trivial: None,
            new: vec![],
            dup: vec![],
            conflict: vec![],
        },
    }
}

/// `uwuwu-cli facts --sync`: только scoped git-коммит sidecar/фактов (без LLM).
/// Зовётся python-оркестратором в конце дня (sidecar python сохраняет сам).
pub fn sync_git(message: &str) -> Result<()> {
    let _guard = LockGuard::acquire()?;
    git_commit(message)
}

fn valid_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 {
        return false;
    }
    if !b.iter().enumerate().all(|(i, c)| match i {
        4 | 7 => *c == b'-',
        _ => c.is_ascii_digit(),
    }) {
        return false;
    }
    // реальность: месяц 01-12, день 01-31 (мусорных каталогов не плодим)
    let num = |from: usize, to: usize| -> u32 { s[from..to].parse().unwrap_or(0) };
    (1..=12).contains(&num(5, 7)) && (1..=31).contains(&num(8, 10))
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// Вердикт — строгое множество (мусорный вердикт не занимает seen-слот).
fn valid_verdict(v: &str) -> bool {
    matches!(v, "duplicate" | "conflict" | "distinct")
}

async fn run_facts_inner(
    text: &str,
    source: &str,
    date: &str,
    stdin: bool,
) -> Result<(FactsOutcome, Vec<FactEntry>)> {
    if !valid_date(date) {
        return Ok((
            FactsOutcome {
                ok: false,
                error: Some("bad_date".into()),
                bad: None,
                trivial: None,
                new: vec![],
                dup: vec![],
                conflict: vec![],
            },
            vec![],
        ));
    }
    if stdin && text.chars().count() < MIN_DIGEST_CHARS {
        return Ok((
            FactsOutcome {
                ok: true,
                trivial: Some(true),
                error: None,
                bad: None,
                new: vec![],
                dup: vec![],
                conflict: vec![],
            },
            vec![],
        ));
    }

    let raw = chat(EXTRACT_SYSTEM, text).await?;
    let (objs, bad) = parse_jsonl(&raw);
    if bad > 0 {
        return Ok((
            FactsOutcome {
                ok: false,
                error: Some("bad_jsonl".into()),
                bad: Some(bad),
                trivial: None,
                new: vec![],
                dup: vec![],
                conflict: vec![],
            },
            vec![],
        ));
    }

    let candidates: Vec<Candidate> = objs
        .iter()
        .filter_map(|o| {
            let st = sanitize(o.get("statement")?.as_str()?.trim());
            let title = sanitize(o.get("title")?.as_str()?.trim());
            if st.chars().count() < STMT_MIN_CHARS || title.is_empty() {
                return None;
            }
            let conf = o.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.6);
            let conf = if (0.1..=1.0).contains(&conf) {
                conf
            } else {
                0.6
            };
            Some(Candidate {
                statement: truncate_chars(&st, STMT_MAX_CHARS),
                title: truncate_chars(&title, TITLE_MAX_CHARS),
                tags: o
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|t| t.as_str())
                            .map(|t| {
                                t.to_lowercase()
                                    .chars()
                                    .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                                    .take(TAG_MAX_CHARS)
                                    .collect::<String>()
                            })
                            .filter(|t| !t.is_empty())
                            .take(MAX_TAGS)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
                confidence: (conf * 100.0).round() / 100.0,
            })
        })
        .take(MAX_FACTS_PER_CALL)
        .collect();

    // Пустой валидный выхлоп — тоже результат (нет фактов), ok:true.
    let mut outcome = FactsOutcome {
        ok: true,
        error: None,
        bad: None,
        trivial: Some(false),
        new: vec![],
        dup: vec![],
        conflict: vec![],
    };
    if candidates.is_empty() {
        return Ok((outcome, vec![]));
    }

    let _guard = LockGuard::acquire()?;
    let root = wiki_root();
    let mut store = FactStore::load(&root)?;
    let month = format!("{}-{}", &date[0..4], &date[5..7]);
    // Новые факты этого прогона — кандидаты для публикации в AFFiNE-бакет дня
    let mut affine_facts: Vec<FactEntry> = Vec::new();

    for f in candidates {
        let s = sha1_norm(&f.statement);
        if store.removed.contains(&s) {
            continue; // tombstone: удалённый dreaming'ом мусор не воскрешаем
        }
        if let Some(existing) = store.by_sha.get(&s) {
            append_source(&existing.path, source, &today())?;
            outcome.dup.push(existing.slug.clone());
            continue;
        }
        let js = shingles(&f.statement);
        let mut scored: Vec<(f32, String)> = store
            .by_sha
            .iter()
            .map(|(k, v)| (jaccard(&js, &v.shingles), k.clone()))
            .filter(|(sim, _)| *sim >= JACCARD_THRESHOLD)
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let olds: Vec<(String, String)> = scored
            .into_iter()
            .take(5)
            .filter_map(|(_, k)| store.by_sha.get(&k).map(|v| (k, v.statement.clone())))
            .collect();

        let mut verd: HashMap<String, String> = HashMap::new();
        if !olds.is_empty() {
            let pairs: Vec<(&Candidate, &String)> = olds.iter().map(|(_, st)| (&f, st)).collect();
            // ключи для мапы после вердиктов
            let keys: Vec<String> = olds.iter().map(|(k, _)| k.clone()).collect();
            let vmap = verdicts(&pairs).await?;
            for (i, k) in keys.iter().enumerate() {
                verd.insert(
                    k.clone(),
                    vmap.get(&i).cloned().unwrap_or_else(|| "distinct".into()),
                );
            }
        }
        let conflict_k = verd
            .iter()
            .find(|(_, v)| v.as_str() == "conflict")
            .map(|(k, _)| k.clone());
        let dup_k = verd
            .iter()
            .find(|(_, v)| v.as_str() == "duplicate")
            .map(|(k, _)| k.clone());

        if let (Some(dk), None) = (dup_k, &conflict_k) {
            let path = store.by_sha[&dk].path.clone();
            append_source(&path, source, &today())?;
            outcome.dup.push(store.by_sha[&dk].slug.clone());
            continue;
        }

        let slug = store.unique_slug(&slugify(&f.title));
        let conflict_slug = conflict_k.as_ref().map(|k| store.by_sha[k].slug.clone());
        let path = write_fact(
            &root,
            &month,
            &slug,
            &f,
            source,
            date,
            &conflict_k,
            &conflict_slug,
        )?;
        if let (Some(ck), Some(cslug)) = (&conflict_k, &conflict_slug) {
            let old_path = store.by_sha[ck].path.clone();
            append_source(&old_path, source, &today())?;
            add_conflict_link(&old_path, &s, cslug, &today())?;
            outcome.conflict.push(ConflictPair {
                slug: slug.clone(),
                with: cslug.clone(),
            });
        }
        store.by_sha.insert(
            s,
            ExistingFact {
                path,
                slug: slug.clone(),
                statement: f.statement.clone(),
                shingles: js,
            },
        );
        store.by_slug.insert(slug.clone());
        affine_facts.push(FactEntry {
            slug: slug.clone(),
            date: date.to_string(),
            title: f.title.clone(),
            statement: f.statement.clone(),
            tags: f.tags.clone(),
            confidence: format!("{:.2}", f.confidence),
            sources: vec![source.to_string()],
        });
        outcome.new.push(NewFact {
            slug,
            title: f.title.clone(),
        });
    }

    let msg = format!(
        "facts: {source} (+{} new, {} dup, {} conflict)",
        outcome.new.len(),
        outcome.dup.len(),
        outcome.conflict.len()
    );
    let _ = git_commit(&msg); // диагностика внутри; фейл коммита не роняет результат
    Ok((outcome, affine_facts))
}

/// Best-effort публикация новых фактов в дейли-бакеты AFFiNE (opt-in через
/// UWUWU_AFFINE_PUBLISH=1). Ошибки шлюза не влияют на исход пайплайна:
/// ФС остаётся source of truth.
async fn publish_to_affine(root: &Path, facts: &[FactEntry]) {
    let cfg = match AffineConfig::from_env() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("warn: affine publish skipped: {e:#}");
            return;
        }
    };
    match crate::affine_publish::publish_facts(root, &cfg, facts).await {
        Ok(count) => eprintln!("affine: {count} facts published to daily buckets"),
        Err(e) => eprintln!("warn: affine publish failed: {e:#}"),
    }
}

// ---------- структуры ----------

struct Candidate {
    statement: String,
    title: String,
    tags: Vec<String>,
    confidence: f64,
}

struct ExistingFact {
    path: PathBuf,
    slug: String,
    statement: String,
    shingles: std::collections::HashSet<String>,
}

struct FactStore {
    by_sha: HashMap<String, ExistingFact>,
    by_slug: std::collections::HashSet<String>,
    removed: std::collections::HashSet<String>,
}

impl FactStore {
    fn load(root: &Path) -> Result<Self> {
        let mut store = FactStore {
            by_sha: HashMap::new(),
            by_slug: std::collections::HashSet::new(),
            removed: std::collections::HashSet::new(),
        };
        let tomb = root.join("facts.removed.json");
        if tomb.exists() {
            if let Ok(arr) = serde_json::from_str::<Vec<String>>(
                &std::fs::read_to_string(&tomb).unwrap_or_default(),
            ) {
                store.removed = arr.into_iter().collect();
            }
        }
        for path in sorted_md(root.join("facts")) {
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some((_, statement)) = split_fact(&body) else {
                continue;
            };
            let s = sha1_norm(&statement);
            store.by_slug.insert(
                path.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            );
            store.by_sha.insert(
                s,
                ExistingFact {
                    path,
                    slug: String::new(),
                    statement,
                    shingles: Default::default(),
                },
            );
            // slug/shingles заполняем после вставки ключа
        }
        for v in store.by_sha.values_mut() {
            v.slug = v
                .path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            v.shingles = shingles(&v.statement);
        }
        Ok(store)
    }

    fn unique_slug(&self, base: &str) -> String {
        let mut slug = base.to_string();
        let mut i = 2;
        while self.by_slug.contains(&slug) {
            slug = format!("{base}-{i}");
            i += 1;
        }
        slug
    }
}

// ---------- LLM ----------

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: String,
}

/// Chat-вызов ollama: python-паритет (timeout 300с + ретрай ×1, think=false,
/// temp/seed/num_predict фиксированы). НЕ embed.rs-стиль (там нет ни того, ни другого).
async fn chat(system: &str, user: &str) -> Result<String> {
    let body = serde_json::json!({
        "model": MODEL,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "stream": false,
        "think": false,
        "options": {
            "temperature": TEMP, "seed": SEED, "num_predict": NUM_PREDICT,
        },
    });
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(CALL_TIMEOUT_SECS))
        .build()?;
    let mut last: Option<String> = None;
    for _ in 0..2 {
        match client
            .post(format!("{OLLAMA_URL}/api/chat"))
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => {
                let parsed: Result<ChatResponse> = resp
                    .json()
                    .await
                    .map_err(|e| anyhow!("ollama chat parse: {e}"));
                match parsed {
                    Ok(r) => return Ok(r.message.content),
                    Err(e) => last = Some(e.to_string()),
                }
            }
            Err(e) => last = Some(format!("ollama connect: {e}")),
        }
    }
    Err(anyhow!("OLLAMA_UNREACHABLE: {}", last.unwrap_or_default()))
}

/// JSONL-парсер: strip <think>/fences; (валидные объекты, число битых строк).
fn parse_jsonl(raw: &str) -> (Vec<serde_json::Value>, usize) {
    let stripped = strip_think(raw);
    let mut objs = Vec::new();
    let mut bad = 0usize;
    for line in stripped.lines() {
        let line = line.trim().trim_end_matches(',').trim();
        if !line.starts_with('{') {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(v) if v.is_object() => objs.push(v),
            _ => bad += 1,
        }
    }
    (objs, bad)
}

fn strip_think(raw: &str) -> String {
    static RE_THINK: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static RE_FENCE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let think = RE_THINK.get_or_init(|| regex::Regex::new(r"(?s)<think>.*?</think>").unwrap());
    let fence = RE_FENCE.get_or_init(|| regex::Regex::new(r"```(?:json)?").unwrap());
    fence
        .replace_all(&think.replace_all(raw, ""), "")
        .to_string()
}

/// Вердикты пачками ≤VERDICT_BATCH: {индекс_пары → duplicate|conflict|distinct}.
/// Пропущенная парой — distinct (python-паритет), с warn в stderr.
async fn verdicts(pairs: &[(&Candidate, &String)]) -> Result<HashMap<usize, String>> {
    let mut result = HashMap::new();
    for chunk_start in (0..pairs.len()).step_by(VERDICT_BATCH) {
        let chunk = &pairs[chunk_start..(chunk_start + VERDICT_BATCH).min(pairs.len())];
        let listing = chunk
            .iter()
            .enumerate()
            .map(|(i, (new, old))| {
                format!(
                    "ПАРА {}: A[{}] ||| B[{}]",
                    i + 1,
                    truncate_chars(&new.statement, 300),
                    truncate_chars(old, 300)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let raw = chat(VERDICT_SYSTEM, &listing).await?;
        let (objs, bad) = parse_jsonl(&raw);
        if bad > 0 {
            eprintln!("warn: вердикт-вызов: {bad} битых строк");
        }
        let mut seen = std::collections::HashSet::new();
        for o in &objs {
            let Some(idx) = o.get("pair").and_then(|v| v.as_u64()) else {
                continue;
            };
            let Some(v) = o.get("verdict").and_then(|v| v.as_str()) else {
                continue;
            };
            let v = v.to_lowercase();
            if idx >= 1 && idx as usize <= chunk.len() && valid_verdict(&v) {
                result.insert(chunk_start + idx as usize - 1, v);
                seen.insert(idx);
            }
        }
        for i in 1..=chunk.len() {
            if !seen.contains(&(i as u64)) {
                eprintln!("warn: вердикт пары {} пропущен → distinct", chunk_start + i);
                result.insert(chunk_start + i - 1, "distinct".into());
            }
        }
    }
    Ok(result)
}

// ---------- текстовые примитивы (порт python 1:1) ----------

fn sha1_norm(text: &str) -> String {
    let norm: String = text
        .nfkc()
        .flat_map(|c| c.to_lowercase())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut h = Sha1::new();
    h.update(norm.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn slugify(title: &str) -> String {
    // python-паритет: NFKD + ascii-ignore (не NFKC)
    let mut out = String::new();
    let mut prev_dash = true;
    for c in title.nfkd() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let slug = out.trim_matches('-').to_string();
    if slug.is_empty() {
        "fact".into()
    } else {
        slug
    }
}

/// Биграммы слов (совместимы с python: [a-zа-яё0-9]+ токены).
fn shingles(text: &str) -> std::collections::HashSet<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"[a-zа-яё0-9]+").unwrap());
    let words: Vec<String> = re
        .find_iter(&text.to_lowercase())
        .map(|m| m.as_str().to_string())
        .collect();
    let mut set = std::collections::HashSet::new();
    for i in 0..words.len().saturating_sub(1).max(1) {
        if i + 1 < words.len() {
            set.insert(format!("{} {}", words[i], words[i + 1]));
        } else if words.len() == 1 {
            set.insert(words[i].clone());
        }
    }
    set
}

fn jaccard(a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    let union = a.union(b).count();
    inter as f32 / union as f32
}

fn ipv6_sub(s: &str) -> bool {
    // Время (12:30:45) — только цифры/двоеточия; IPv6 содержит hex-буквы или 3+ colon
    s.chars()
        .any(|c| c.is_ascii_hexdigit() && !c.is_ascii_digit())
        || s.matches(':').count() >= 3
}

/// Санитайзинг адресов/хостов (порт python-регексов): <ip>/<host>.
fn sanitize(text: &str) -> String {
    static V4: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static V6: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static FQDN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let v4 = V4.get_or_init(|| regex::Regex::new(r"\b(?:\d{1,3}\.){3}\d{1,3}\b").unwrap());
    let v6 = V6.get_or_init(|| {
        regex::Regex::new(
            r"\b(?:[0-9a-fA-F]{1,4}:){2,7}[0-9a-fA-F]{1,4}\b|\b(?:[0-9a-fA-F]{1,4}:)+:(?:[0-9a-fA-F]{1,4}:?)*|\b:(?::[0-9a-fA-F]{1,4})+",
        ).unwrap()
    });
    let fqdn = FQDN.get_or_init(|| {
        regex::Regex::new(
            r"(?i)\b(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.)+(?:ru|com|net|io|dev|test|local|games|app|org|ai)\b",
        ).unwrap()
    });
    let text = v4.replace_all(text, "<ip>").to_string();
    let text = v6
        .replace_all(&text, |c: &regex::Captures| {
            let m = c.get(0).map(|g| g.as_str()).unwrap_or_default();
            if ipv6_sub(m) {
                "<ip>".to_string()
            } else {
                m.to_string()
            }
        })
        .to_string();
    fqdn.replace_all(&text, "<host>").to_string()
}

// ---------- io ----------

fn sorted_md(dir: PathBuf) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_md(&dir, &mut out);
    out.sort();
    out
}

fn collect_md(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_md(&p, out);
        } else if p.extension().map(|x| x == "md").unwrap_or(false) {
            out.push(p);
        }
    }
}

/// frontmatter/тело по первому `\n---\n` (python split_fact).
fn split_fact(body: &str) -> Option<(String, String)> {
    if let Some(rest) = body.strip_prefix("---\n") {
        if let Some(idx) = rest.find("\n---\n") {
            let fm = rest[..idx].to_string();
            let st = rest[idx + 5..].trim().to_string();
            if !st.is_empty() {
                return Some((fm, st));
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn write_fact(
    root: &Path,
    month: &str,
    slug: &str,
    f: &Candidate,
    source: &str,
    date: &str,
    conflict_k: &Option<String>,
    conflict_slug: &Option<String>,
) -> Result<PathBuf> {
    let dir = root.join("facts").join(month);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{slug}.md"));
    let mut fm = format!(
        "---\nupdated: {date}\ntitle: {}\ndescription: {}\nconfidence: {}\n\
         sources: [{source}]\ntags: [{}]\nstatus: {}\n",
        f.title,
        truncate_chars(&f.statement, DESC_MAX_CHARS).replace('\n', " "),
        f.confidence,
        if f.tags.is_empty() {
            "fact".to_string()
        } else {
            f.tags.join(", ")
        },
        if conflict_k.is_some() {
            "conflict"
        } else {
            "accepted"
        },
    );
    if let Some(ck) = conflict_k {
        fm.push_str(&format!("conflicts: [{ck}]\n"));
    }
    fm.push_str("---\n\n");
    // эскейп markdown-hr (python-паритет)
    let hr = regex::Regex::new(r"(?m)^\s*-{3,}\s*$").unwrap();
    let mut body = hr.replace_all(&f.statement, "—").to_string();
    if let Some(cs) = conflict_slug {
        body.push_str(&format!("\n\n[conflict: {cs}]"));
    }
    std::fs::write(&path, format!("{fm}{body}\n"))?;
    Ok(path)
}

fn append_source(path: &Path, uri: &str, date: &str) -> Result<()> {
    let body = std::fs::read_to_string(path)?;
    let (fm, statement) =
        split_fact(&body).ok_or_else(|| anyhow!("bad fact: {}", path.display()))?;
    let mut lines: Vec<String> = Vec::new();
    for line in fm.lines() {
        if line.starts_with("sources: [") && !line.contains(uri) {
            let inner = line["sources: [".len()..]
                .trim_end_matches(']')
                .trim()
                .to_string();
            let sep = if inner.is_empty() { "" } else { ", " };
            lines.push(format!("sources: [{inner}{sep}{uri}]"));
        } else if line.starts_with("updated: ") {
            lines.push(format!("updated: {date}"));
        } else {
            lines.push(line.to_string());
        }
    }
    std::fs::write(
        path,
        format!("---\n{}\n---\n{statement}\n", lines.join("\n")),
    )?;
    Ok(())
}

fn add_conflict_link(path: &Path, other_sha: &str, other_slug: &str, date: &str) -> Result<()> {
    let body = std::fs::read_to_string(path)?;
    let (fm, mut statement) =
        split_fact(&body).ok_or_else(|| anyhow!("bad fact: {}", path.display()))?;
    let status = regex::Regex::new(r"(?m)^status: .*$").unwrap();
    let mut new_fm = status.replace(&fm, "status: conflict").to_string();
    let conflicts_line = regex::Regex::new(r"(?m)^conflicts: \[(.*)\]$").unwrap();
    if let Some(c) = conflicts_line.captures(&new_fm) {
        let whole = c.get(0).unwrap().as_str().to_string();
        let inner = c.get(1).map(|m| m.as_str().trim()).unwrap_or_default();
        if !inner.contains(other_sha) {
            let sep = if inner.is_empty() { "" } else { ", " };
            new_fm = new_fm.replace(&whole, &format!("conflicts: [{inner}{sep}{other_sha}]"));
        }
    } else {
        new_fm.push_str(&format!("conflicts: [{other_sha}]\n"));
    }
    if !statement.contains(&format!("[conflict: {other_slug}")) {
        statement.push_str(&format!("\n\n[conflict: {other_slug}]"));
    }
    let updated = regex::Regex::new(r"(?m)^updated: .*$").unwrap();
    let new_fm = updated
        .replace(&new_fm, format!("updated: {date}"))
        .to_string();
    std::fs::write(
        path,
        format!("---\n{}\n---\n{statement}\n", new_fm.trim_end()),
    )?;
    Ok(())
}

/// Scoped git: ТОЛЬКО факты и sidecar-файлы (все 4 пути — Rust единственный коммитер).
fn git_commit(message: &str) -> Result<()> {
    let root = wiki_root();
    let paths = [
        "facts/",
        "facts.ing.json",
        "facts.removed.json",
        "facts.retries.json",
    ];
    let existing: Vec<&str> = paths
        .iter()
        .copied()
        .filter(|p| root.join(p).exists())
        .collect();
    let add = std::process::Command::new("git")
        .args(["add"])
        .args(&existing)
        .current_dir(&root)
        .output()?;
    if !add.status.success() {
        eprintln!(
            "warn: git add failed: {}",
            String::from_utf8_lossy(&add.stderr)
        );
        return Ok(());
    }
    let commit = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=uwuwu",
            "-c",
            "user.email=uwuwu@local",
            "commit",
            "-qm",
            message,
        ])
        .current_dir(&root)
        .output()?;
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&commit.stdout),
        String::from_utf8_lossy(&commit.stderr)
    );
    if !commit.status.success()
        && !out.contains("nothing to commit")
        && !out.contains("nothing added to commit")
        && !out.contains("no changes added to commit")
    {
        eprintln!("warn: git commit failed: {out}");
    }
    Ok(())
}

// ---------- lock ----------

/// Файловый lock: держатель touch'ит mtime каждые LOCK_TOUCH_SECS (фоновая
/// задача); lock старше LOCK_STALE_SECS считается брошенным (pid внутри —
/// для диагностики). Используется и миграцией affine (конкурент с facts
/// на affine_map.json).
pub(crate) struct LockGuard {
    path: PathBuf,
    touch: Option<tokio::task::JoinHandle<()>>,
}

impl LockGuard {
    pub(crate) fn acquire() -> Result<Self> {
        let path = wiki_root().join(LOCK_PATH);
        // env-вики (UWUWU_WIKI) может ещё не существовать — создаём корень
        // до lock-цикла, иначе create_new возвращает NotFound навсегда.
        std::fs::create_dir_all(&wiki_root())
            .map_err(|e| anyhow!("wiki_root create_dir_all({:?}): {}", wiki_root(), e))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            // Дедлайн наверху тела: ни одна ветка (включая stale-continue)
            // не имеет права обходить его — иначе вечный цикл.
            if std::time::Instant::now() > deadline {
                return Err(anyhow!("facts.lock: не дождались освобождения за 60с"));
            }
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(f) => {
                    let pid = std::process::id();
                    let _ = std::fs::write(&path, pid.to_string());
                    drop(f);
                    let p = path.clone();
                    let touch = tokio::spawn(async move {
                        loop {
                            tokio::time::sleep(std::time::Duration::from_secs(LOCK_TOUCH_SECS))
                                .await;
                            // touch mtime
                            if let Ok(f) = std::fs::OpenOptions::new().append(true).open(&p) {
                                drop(f);
                                let now = filetime::FileTime::now();
                                let _ = filetime::set_file_times(&p, now, now);
                            }
                        }
                    });
                    return Ok(LockGuard {
                        path,
                        touch: Some(touch),
                    });
                }
                Err(_) => {
                    // stale-детект: mtime старше порога. Повторная проверка после
                    // remove-паузы уменьшает окно гонки «снёс свежий lock победителя»:
                    // после remove идёт create_new — чужой свежий файл даст Err → ждём.
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .map(|age| age.as_secs() > LOCK_STALE_SECS)
                        .unwrap_or(true);
                    if stale {
                        let mtime_before = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                        std::thread::sleep(std::time::Duration::from_millis(150));
                        let still = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                        if mtime_before == still {
                            let _ = std::fs::remove_file(&path);
                        }
                        continue;
                    }
                    // sync-CLI: thread::sleep осознан (вызовы из не-async sync_git)
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
            }
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Some(t) = self.touch.take() {
            t.abort();
        }
        // Удаляем только свой lock (pid-сверка): чужой — не трогаем
        let mine = std::fs::read_to_string(&self.path)
            .map(|s| s.trim() == std::process::id().to_string())
            .unwrap_or(false);
        if mine {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Чтение текста из --stdin или аргументов.
pub fn read_text(use_stdin: bool, args: &[String]) -> Result<String> {
    if use_stdin {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        Ok(buf)
    } else if !args.is_empty() {
        Ok(args.join(" "))
    } else {
        Err(anyhow!(
            "нет текста: укажите --stdin или передайте текст аргументом"
        ))
    }
}
