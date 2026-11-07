//! Резолв слага `read` в docId ядра SiYuan.
//!
//! Формы слага: `<project>/<doc>` (ноутбук Projects), `<topic>/<doc>`
//! (Experience), одиночный слаг дневника (Daily), одиночный заголовок дока.
//! Дерево резолва: точный hpath-матч `/{slug}` по всем ноутбукам; фолбэк по
//! заголовку (последний сегмент слага, прежнее поведение); на промахе —
//! like-кандидаты по hpath ступенями (контекст проектной части слага →
//! глобальный contains → word-AND «каждое слово сегмента содержится в hpath»):
//! один кандидат — авторезолв с пометкой в stderr, несколько — отказ со
//! списком (hpath, ноутбук, docId — docId даёт агенту готовый обход find →
//! export).

use crate::siyuan::SiYuan;
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::HashMap;

/// Кандидатов в подсказке; запрос идёт с лимитом+1 — чтобы честно пометить
/// обрезку вместо молчаливой потери хвостов (docs_ls-урок).
const CANDIDATE_LIMIT: usize = 5;

/// Слаг → docId дока (export по нему делает вызывающий).
pub(crate) async fn resolve(sy: &SiYuan, slug_raw: &str) -> Result<String> {
    let slug = slug_raw.trim_start_matches('/');

    // 1) точный матч слага как полного hpath: /<project>/<doc>, /<topic>/<doc>, /<daily-slug>
    let mut exact = doc_blocks(sy, &format!("hpath = {}", sql_str(&format!("/{slug}")))).await?;
    if exact.len() > 1 {
        return Err(anyhow!(ambiguous(slug_raw, &exact, sy).await));
    }
    if let Some(doc) = exact.pop() {
        return Ok(doc.id);
    }

    // 2) фолбэк: последний сегмент слага = заголовок дока
    let title = slug.rsplit('/').next().unwrap_or(slug);
    let mut by_title = doc_blocks(sy, &format!("content = {}", sql_str(title))).await?;
    match by_title.len() {
        1 => return Ok(by_title.pop().expect("len checked above").id),
        0 => {}
        _ => return Err(anyhow!(ambiguous(slug_raw, &by_title, sy).await)),
    }

    // 3) промах: пустой последний сегмент (слаг с хвостовым «/») — like-матч
    //    схлопнулся бы в «всё подряд», уходим в голый отказ без запросов
    if title.is_empty() {
        return Err(anyhow!("док «{slug_raw}» не найден"));
    }
    let candidates = like_candidates(sy, slug, title).await?;
    if candidates.hits.len() == 1 {
        let hit = &candidates.hits[0];
        eprintln!(
            "read: слаг «{slug_raw}» — найден единственный похожий док: {}",
            describe_hit(hit, &notebook_names(sy).await.unwrap_or_default())
        );
        return Ok(hit.id.clone());
    }
    if candidates.hits.is_empty() {
        return Err(anyhow!("док «{slug_raw}» не найден"));
    }
    Err(anyhow!(similar_message(slug_raw, &candidates, sy).await))
}

/// like-кандидаты на промахе, ступени (первая непустая даёт результат):
/// контекст слага (проектная часть как hpath-префикс) → глобальный contains
/// → word-AND (каждое слово сегмента в hpath — ловит серединные расхождения).
struct Candidates {
    hits: Vec<DocHit>,
    /// Упёрлись в CANDIDATE_LIMIT — список неполный (пометка в отказе).
    truncated: bool,
}

async fn like_candidates(sy: &SiYuan, slug: &str, seg: &str) -> Result<Candidates> {
    let head = slug.strip_suffix(seg).unwrap_or(slug);
    let mut hits = if head.is_empty() {
        doc_blocks(sy, &hpath_condition(seg, "")).await?
    } else {
        let scoped = doc_blocks(sy, &hpath_condition(seg, head)).await?;
        if scoped.is_empty() {
            // проектная часть слага не совпала — глобальный фолбэк
            doc_blocks(sy, &hpath_condition(seg, "")).await?
        } else {
            scoped
        }
    };
    if hits.is_empty() {
        // хвостовая ступень: hpath содержит каждое слово сегмента — ловит
        // одно-символьные расхождения в середине поверенного тайтла
        // («хаускип» ⊂ «хаускипа») и перестановки слов
        if let Some(cond) = words_condition(seg) {
            hits = doc_blocks(sy, &cond).await?;
        }
    }
    let truncated = hits.len() > CANDIDATE_LIMIT;
    hits.truncate(CANDIDATE_LIMIT);
    Ok(Candidates { hits, truncated })
}

/// WHERE-фрагмент like-матча по hpath: детерминированный порядок, лимит+1
/// (детект обрезки). Вайлкарды LIKE в сегменте экранируются (`escape '\'`),
/// как в wiki_ops-гарде ADR-номера — матч строго по литеральному тексту слага.
fn hpath_condition(seg: &str, head: &str) -> String {
    let pattern = if head.is_empty() {
        format!("%{}%", like_escape(seg))
    } else {
        format!("/{}%{}%", like_escape(head), like_escape(seg))
    };
    like_stmt("hpath", &pattern)
}

/// WHERE-фрагмент «hpath содержит каждое слово сегмента»; None — сегмент
/// однословный (ступень эквивалентна предыдущей, дублировать запрос нет смысла).
fn words_condition(seg: &str) -> Option<String> {
    let words: Vec<&str> = seg.split_whitespace().collect();
    if words.len() < 2 {
        return None;
    }
    let cond = words
        .iter()
        .map(|word| {
            format!(
                "hpath like {} escape '\\'",
                sql_str(&format!("%{}%", like_escape(word)))
            )
        })
        .collect::<Vec<_>>()
        .join(" and ");
    Some(format!(
        "{cond} order by hpath limit {}",
        CANDIDATE_LIMIT + 1
    ))
}

fn like_stmt(column: &str, pattern: &str) -> String {
    format!(
        "{column} like {} escape '\\' order by hpath limit {}",
        sql_str(pattern),
        CANDIDATE_LIMIT + 1
    )
}

/// Экранирование вайлкардов LIKE в литеральном тексте (`\` первым);
/// намеренные `%` паттерна добавляются сборкой после экранирования.
fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

struct DocHit {
    id: String,
    hpath: String,
    box_id: String,
}

/// Док-блоки (type='d') по готовому WHERE-фрагменту.
async fn doc_blocks(sy: &SiYuan, condition: &str) -> Result<Vec<DocHit>> {
    let rows = sy
        .sql(&format!(
            "select id, hpath, box from blocks where type='d' and {condition}"
        ))
        .await?;
    Ok(rows.iter().map(doc_hit).collect())
}

fn doc_hit(row: &Value) -> DocHit {
    DocHit {
        id: str_field(row, "id"),
        hpath: str_field(row, "hpath"),
        box_id: str_field(row, "box"),
    }
}

/// «<hpath> (ноутбук: <имя>, id: <docId>)» — формат строки кандидата.
fn describe_hit(hit: &DocHit, notebook_by_id: &HashMap<String, String>) -> String {
    let notebook = notebook_by_id
        .get(&hit.box_id)
        .map(String::as_str)
        .unwrap_or("?");
    format!("{} (ноутбук: {}, id: {})", hit.hpath, notebook, hit.id)
}

/// Строки списка кандидатов; недоступность справочника ноутбуков не роняет подсказку.
async fn hit_lines(hits: &[DocHit], sy: &SiYuan) -> Vec<String> {
    let names = notebook_names(sy).await.unwrap_or_default();
    hits.iter()
        .map(|hit| format!("  - {}", describe_hit(hit, &names)))
        .collect()
}

/// Сообщение об неоднозначности точного матча: каждый кандидат с hpath,
/// именем ноутбука и docId.
async fn ambiguous(slug: &str, hits: &[DocHit], sy: &SiYuan) -> String {
    let mut msg = format!("док «{slug}» неоднозначен ({} кандидатов):", hits.len());
    for line in hit_lines(hits, sy).await {
        msg.push('\n');
        msg.push_str(&line);
    }
    msg
}

/// Отказ на промахе со списком похожих доков (like-подсказка).
async fn similar_message(slug: &str, candidates: &Candidates, sy: &SiYuan) -> String {
    let mut msg = format!("док «{slug}» не найден — похожие доки:");
    for line in hit_lines(&candidates.hits, sy).await {
        msg.push('\n');
        msg.push_str(&line);
    }
    if candidates.truncated {
        msg.push_str(&format!(
            "\n  … показаны первые {CANDIDATE_LIMIT}, кандидатов больше — уточни слаг"
        ));
    }
    msg
}

/// id ноутбука → имя (для перечня кандидатов).
async fn notebook_names(sy: &SiYuan) -> Result<HashMap<String, String>> {
    let mut names = HashMap::new();
    for nb in sy.notebooks().await? {
        if let (Some(id), Some(name)) = (
            nb.get("id").and_then(|v| v.as_str()),
            nb.get("name").and_then(|v| v.as_str()),
        ) {
            names.insert(id.to_string(), name.to_string());
        }
    }
    Ok(names)
}

/// Строка в SQL-литерал с экранированием одинарной кавычки.
fn sql_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn str_field(row: &Value, key: &str) -> String {
    row.get(key).and_then(|v| v.as_str()).unwrap_or("").into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_str_escapes_single_quotes() {
        assert_eq!(sql_str("it's"), "'it''s'");
        assert_eq!(sql_str("plain"), "'plain'");
    }

    #[test]
    fn like_condition_scopes_by_project_head_and_adds_limit_plus_one() {
        let stmt = hpath_condition("ADR-030", "uwuwu/ADR/");
        assert!(stmt.contains("hpath like '/uwuwu/ADR/%ADR-030%'"), "{stmt}");
        assert!(stmt.contains("escape '\\'"), "{stmt}");
        assert!(stmt.contains("order by hpath"), "{stmt}");
        assert!(stmt.contains("limit 6"), "{stmt}");
    }

    #[test]
    fn like_condition_without_head_is_global() {
        let stmt = hpath_condition("ADR-030", "");
        assert!(stmt.contains("hpath like '%ADR-030%'"), "{stmt}");
        assert!(!stmt.contains("//"), "{stmt}");
    }

    #[test]
    fn like_condition_escapes_like_wildcards_in_segment() {
        // вайлкарды в слаге — литералы, не шаблон (конвенция wiki_ops-гарда)
        let stmt = hpath_condition("100%_done", "uwuwu/");
        assert!(stmt.contains("hpath like '/uwuwu/%100\\%\\_done%'"), "{stmt}");
        assert!(stmt.contains("escape '\\'"), "{stmt}");
    }

    #[test]
    fn words_condition_anchors_every_word_and_skips_single_word() {
        // однословный сегмент: ступень эквивалентна глобальному contains — None
        assert_eq!(words_condition("ADR-030"), None);
        let stmt = words_condition("Фабрика uwuwu: Design хаускип").expect("two+ words");
        assert!(stmt.contains("hpath like '%Фабрика%'"), "{stmt}");
        assert!(stmt.contains("hpath like '%uwuwu:%'"), "{stmt}");
        assert!(stmt.contains("hpath like '%хаускип%'"), "{stmt}");
        assert!(stmt.contains(" and "), "{stmt}");
        assert!(stmt.contains("order by hpath"), "{stmt}");
        assert!(stmt.contains("limit 6"), "{stmt}");
    }

    #[test]
    fn words_condition_escapes_wildcards_in_words() {
        let stmt = words_condition("a b%_c").expect("two+ words");
        assert!(stmt.contains("hpath like '%b\\%\\_c%'"), "{stmt}");
        assert!(stmt.contains("escape '\\'"), "{stmt}");
    }
}
