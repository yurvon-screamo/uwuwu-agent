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
    /// Facts published into AFFiNE daily buckets this run (mandatory stage).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<usize>,
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
/// Publishes into AFFiNE ALWAYS — the workspace is the primary destination,
/// not an optional mirror. A gateway failure fails the whole run (ok:false);
/// the FS copy stays as the local index for dedup/semantic search, and dup
/// facts are re-published too so a retried run converges after a failure.
pub async fn run_facts(text: &str, source: &str, date: &str, stdin: bool) -> FactsOutcome {
    match run_facts_inner(text, source, date, stdin).await {
        Ok((mut out, affine_facts)) => {
            if affine_facts.is_empty() {
                return out;
            }
            // The dedup index has already been saved INSIDE the pipeline
            // lock (see run_facts_inner) — no race window, no lost updates.
            // Publish runs OUTSIDE the lock: network I/O must not starve
            // concurrent runs. A failed publish leaves facts marked in the
            // index, so a retry walks the dup path and republishes them —
            // convergence is guaranteed by republication, not by ordering.
            match publish_to_affine(&wiki_root(), &affine_facts).await {
                Ok(published) => out.published = Some(published),
                Err(e) => {
                    eprintln!("error: affine publish failed: {e:#}");
                    out.ok = false;
                    out.error = Some(format!("affine_publish_failed: {e:#}"));
                }
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
            published: None,
        },
        Err(e) => FactsOutcome {
            ok: false,
            error: Some(format!("{e:#}")),
            bad: None,
            trivial: None,
            new: vec![],
            dup: vec![],
            conflict: vec![],
            published: None,
        },
    }
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

/// Вердикт — строгое множество (мусорный вердикт не занимает seen-слот).
fn valid_verdict(v: &str) -> bool {
    matches!(v, "duplicate" | "conflict" | "distinct")
}

/// Отрицательный исход до дедупа (плохая дата / битый JSONL) — пустой
/// выхлоп без кандидатов.
fn early_fail(error: &str, bad: Option<usize>) -> (FactsOutcome, Vec<FactEntry>) {
    (
        FactsOutcome {
            ok: false,
            error: Some(error.into()),
            bad,
            trivial: None,
            new: vec![],
            dup: vec![],
            conflict: vec![],
            published: None,
        },
        vec![],
    )
}

/// Кандидаты из LLM-выхлопа: санитайзинг, лимиты длин, нормализация тегов
/// и confidence; слабые записи отбрасываются.
fn parse_candidates(objs: &[serde_json::Value]) -> Vec<Candidate> {
    objs.iter()
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
        .collect()
}

/// Дедуп одного кандидата против индекса: tombstone → точный хит →
/// шинглы+вердикты → новая запись (с конфликтом или без). Мутирует индекс,
/// итог прогона и пул на публикацию.
async fn classify_candidate(
    f: Candidate,
    index: &mut crate::fact_index::FactIndex,
    removed: &std::collections::HashSet<String>,
    source: &str,
    date: &str,
    outcome: &mut FactsOutcome,
    affine_facts: &mut Vec<FactEntry>,
) -> Result<()> {
    let s = sha1_norm(&f.statement);
    if removed.contains(&s) {
        return Ok(()); // tombstone: удалённый dreaming'ом мусор не воскрешаем
    }
    if let Some(existing) = index.get_mut(&s) {
        index_touch(existing, source);
        affine_facts.push(indexed_to_fact(existing));
        outcome.dup.push(existing.slug.clone());
        return Ok(());
    }
    let js = shingles(&f.statement);
    let mut scored: Vec<(f32, String)> = index
        .iter()
        .map(|(k, v)| (jaccard(&js, &v.shingles), k.clone()))
        .filter(|(sim, _)| *sim >= JACCARD_THRESHOLD)
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let olds: Vec<(String, String)> = scored
        .into_iter()
        .take(5)
        .filter_map(|(_, k)| index.get_mut(&k).map(|v| (k, v.statement.clone())))
        .collect();

    let mut verd: HashMap<String, String> = HashMap::new();
    if !olds.is_empty() {
        let pairs: Vec<(&Candidate, &String)> = olds.iter().map(|(_, st)| (&f, st)).collect();
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
        if let Some(dup) = index.get_mut(&dk) {
            index_touch(dup, source);
            affine_facts.push(indexed_to_fact(dup));
            outcome.dup.push(dup.slug.clone());
        }
        return Ok(());
    }

    let slug = unique_slug(index, &slugify(&f.title));
    let conflict_slug = conflict_k
        .as_ref()
        .and_then(|k| index.get_mut(k).map(|v| v.slug.clone()));
    if let (Some(ck), Some(cslug)) = (&conflict_k, &conflict_slug) {
        if let Some(old) = index.get_mut(ck) {
            mark_conflict(old, &s, source);
            affine_facts.push(indexed_to_fact(old));
        }
        outcome.conflict.push(ConflictPair {
            slug: slug.clone(),
            with: cslug.clone(),
        });
    }
    index.insert_new(
        s.clone(),
        crate::fact_index::IndexedFact {
            slug: slug.clone(),
            title: f.title.clone(),
            statement: f.statement.clone(),
            date: date.to_string(),
            sources: vec![source.to_string()],
            shingles: js,
            conflicts: Vec::new(),
            status: if conflict_k.is_some() {
                "conflict"
            } else {
                "accepted"
            }
            .into(),
            tags: f.tags.clone(),
            confidence: format!("{:.2}", f.confidence),
        },
    );
    affine_facts.push(FactEntry {
        slug: slug.clone(),
        date: date.to_string(),
        title: f.title.clone(),
        statement: f.statement.clone(),
        tags: f.tags.clone(),
        confidence: format!("{:.2}", f.confidence),
        sources: vec![source.to_string()],
        status: if conflict_k.is_some() {
            "conflict"
        } else {
            "accepted"
        }
        .into(),
    });
    outcome.new.push(NewFact {
        slug,
        title: f.title.clone(),
    });
    Ok(())
}

async fn run_facts_inner(
    text: &str,
    source: &str,
    date: &str,
    stdin: bool,
) -> Result<(FactsOutcome, Vec<FactEntry>)> {
    if !valid_date(date) {
        return Ok(early_fail("bad_date", None));
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
                published: None,
            },
            vec![],
        ));
    }

    let raw = chat(EXTRACT_SYSTEM, text).await?;
    let (objs, bad) = parse_jsonl(&raw);
    if bad > 0 {
        return Ok(early_fail("bad_jsonl", Some(bad)));
    }

    let candidates = parse_candidates(&objs);

    // Пустой валидный выхлоп — тоже результат (нет фактов), ok:true.
    let mut outcome = FactsOutcome {
        ok: true,
        error: None,
        bad: None,
        trivial: Some(false),
        new: vec![],
        dup: vec![],
        conflict: vec![],
        published: None,
    };
    if candidates.is_empty() {
        return Ok((outcome, vec![]));
    }

    let _guard = LockGuard::acquire()?;
    let root = wiki_root();
    let mut index = crate::fact_index::FactIndex::load(&root)?;
    let removed = load_removed(&root);
    // Новые факты этого прогона — кандидаты для публикации в AFFiNE-бакет дня
    let mut affine_facts: Vec<FactEntry> = Vec::new();

    for f in candidates {
        classify_candidate(
            f,
            &mut index,
            &removed,
            source,
            date,
            &mut outcome,
            &mut affine_facts,
        )
        .await?;
    }

    // Persist the dedup index inside the critical section: concurrent runs
    // always load the freshest state, and a failed publish later does not
    // corrupt the dedup baseline (retries republish via the dup path).
    index.save(&root)?;
    Ok((outcome, affine_facts))
}

/// Mandatory AFFiNE publication: new AND dup facts of this run. Dup facts
/// re-published so a retried run converges after a previous publish failure
/// (the map skips facts already visible in the bucket).
async fn publish_to_affine(root: &Path, facts: &[FactEntry]) -> anyhow::Result<usize> {
    let cfg = AffineConfig::from_env()?;
    crate::affine_publish::publish_facts(root, &cfg, facts).await
}

// ---------- структуры ----------

struct Candidate {
    statement: String,
    title: String,
    tags: Vec<String>,
    confidence: f64,
}

// ---------- index helpers ----------

/// Tombstones из facts.removed.json: удалённый dreaming'ом мусор
/// не должен воскресать в новых прогонах.
fn load_removed(root: &std::path::Path) -> std::collections::HashSet<String> {
    std::fs::read_to_string(root.join("facts.removed.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .map(|arr| arr.into_iter().collect())
        .unwrap_or_default()
}

fn unique_slug(index: &crate::fact_index::FactIndex, base: &str) -> String {
    let mut slug = base.to_string();
    let mut i = 2;
    while index.slug_exists(&slug) {
        slug = format!("{base}-{i}");
        i += 1;
    }
    slug
}

/// Источник уже видел этот факт — фиксируем в индексе (аналог бывшего
/// append_source в md-файле).
fn index_touch(fact: &mut crate::fact_index::IndexedFact, source: &str) {
    if !fact.sources.iter().any(|s| s == source) {
        fact.sources.push(source.to_string());
    }
    // date stays the fact's own day — a later duplicate sighting must not
    // relocate the fact to the duplicate run's bucket
}

fn mark_conflict(fact: &mut crate::fact_index::IndexedFact, other_sha: &str, source: &str) {
    fact.status = "conflict".into();
    if !fact.conflicts.iter().any(|c| c == other_sha) {
        fact.conflicts.push(other_sha.to_string());
    }
    index_touch(fact, source);
}

fn indexed_to_fact(fact: &crate::fact_index::IndexedFact) -> FactEntry {
    FactEntry {
        slug: fact.slug.clone(),
        date: fact.date.clone(),
        title: fact.title.clone(),
        statement: fact.statement.clone(),
        tags: fact.tags.clone(),
        confidence: if fact.confidence.is_empty() {
            "-".into()
        } else {
            fact.confidence.clone()
        },
        sources: fact.sources.clone(),
        status: fact.status.clone(),
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

use crate::fact_index::sha1_norm;

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

use crate::fact_index::shingles;

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
        Self::acquire_at(&wiki_root().join(LOCK_PATH))
    }

    /// Lock по явному пути: например, affine_map.lock для писателей
    /// affine_map.json (миграция и publish-хук) — не блокирует facts.lock.
    pub(crate) fn acquire_at(path: &Path) -> Result<Self> {
        let path = path.to_path_buf();
        // каталог lock-файла может ещё не существовать (env-вики) — создаём
        // до lock-цикла, иначе create_new возвращает NotFound навсегда.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow!("lock dir create_dir_all({:?}): {e}", parent))?;
        }
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
                    // удержание лока: thread::sleep осознан (не-async контекст)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_candidates_sanitizes_and_caps() {
        let objs = vec![
            // короче STMT_MIN_CHARS → отбрасывается
            json!({"statement": "short", "title": "t", "tags": [], "confidence": 0.9}),
            json!({"statement": "Достаточно длинное утверждение для факта", "title": "Valid Title",
                   "tags": ["Rust-Lang", "с русским", "x"], "confidence": 9.0}),
        ];
        let out = parse_candidates(&objs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Valid Title");
        // не-ascii тег выпадает, дефис сохраняется
        assert_eq!(out[0].tags, vec!["rust-lang".to_string(), "x".to_string()]);
        // невалидный confidence (9.0 вне 0.1..=1.0) → дефолт 0.6
        assert_eq!(out[0].confidence, 0.6);
    }

    #[test]
    fn parse_candidates_takes_max_per_call() {
        let objs: Vec<_> = (0..(MAX_FACTS_PER_CALL + 3))
            .map(|i| {
                json!({"statement": format!("Утверждение номер {i} достаточно длинное"), "title": format!("T{i}"),
                       "tags": [], "confidence": 0.8})
            })
            .collect();
        assert_eq!(parse_candidates(&objs).len(), MAX_FACTS_PER_CALL);
    }

    #[test]
    fn read_text_joins_args_or_reads_stdin() {
        let args = vec!["a".to_string(), "b".to_string()];
        assert_eq!(read_text(false, &args).unwrap(), "a b");
        assert!(read_text(false, &[]).is_err());
    }
}
