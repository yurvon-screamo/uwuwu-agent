//! search/semantic: полнотекст + секции доков + семантика.

use crate::cmd::cli::SemanticSub;
use crate::siyuan::SiYuan;
use anyhow::Result;
use serde_json::Value;

// ——— docs/search/blob/projects ——— (search; read.rs — соседний модуль)

/// Голова stdout search — контракт фабричных агентов (парсят вывод);
/// единственная точка формата, правка = красный golden-тест.
pub(super) fn search_head(query: &str) -> String {
    format!("Searching for: \"{query}\"")
}

/// Потолок фетча доков для doc-секции: запас от текущего размера
/// воркспейса (1642 дока на 07.11); свыше — тихий пропуск хвоста.
const DOC_TITLES_CAP: usize = 5000;

/// Заголовок doc-секции search.
const DOC_SECTION_HEADER: &str = "Доки (по hpath):";

/// Строки полнотекстовой секции — формат байт-в-байт прежний:
/// вывод парсят фабричные агенты (контракт регрессии 20261104072044,
/// golden-тест локает).
pub(crate) fn fulltext_lines(rows: &[Value]) -> Vec<String> {
    let mut lines = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let hpath = r.get("hpath").and_then(|h| h.as_str()).unwrap_or("");
        let content = r.get("content").and_then(|c| c.as_str()).unwrap_or("");
        lines.push(format!("{}. {}", i + 1, hpath));
        lines.push(format!("    {}", content.replace('\n', " ")));
    }
    if rows.is_empty() {
        lines.push("ничего не найдено".to_string());
    }
    lines
}

/// Секция «доки по hpath»: заголовки, которых контентный like не видит.
/// Матч в Rust — SQLite LIKE регистрозависим для кириллицы (проба 07.11:
/// lowercase-запрос даёт пусто). Ранжирование: точный подстрочный матч
/// всего запроса раньше AND-токенов; пустой матч → секция не печатается.
pub(crate) fn doc_section_lines(query: &str, hpaths: &[String], limit: usize) -> Vec<String> {
    let tokens: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    let whole_query = tokens.join(" ");
    let mut exact = Vec::new();
    let mut by_tokens = Vec::new();
    for hpath in hpaths {
        let lower = hpath.to_lowercase();
        if lower.contains(&whole_query) {
            exact.push(hpath);
        } else if tokens.iter().all(|t| lower.contains(t.as_str())) {
            by_tokens.push(hpath);
        }
    }
    let hits: Vec<&String> = exact.into_iter().chain(by_tokens).take(limit).collect();
    if hits.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![DOC_SECTION_HEADER.to_string()];
    for (i, hpath) in hits.iter().enumerate() {
        lines.push(format!("{}. {hpath}", i + 1));
    }
    lines
}

/// Маппинг фетча доков в строки секции: отказ ядра — статус внутри
/// секции, команда не падает (дисциплина semantic-секции).
pub(crate) fn doc_section_result_lines(
    query: &str,
    limit: usize,
    fetch: Result<Vec<String>>,
) -> Vec<String> {
    match fetch {
        Ok(hpaths) => doc_section_lines(query, &hpaths, limit),
        Err(e) => vec![
            DOC_SECTION_HEADER.to_string(),
            format!("kernel sql недоступен: {e:#}"),
        ],
    }
}

/// Сборка stdout search между головой и SOFA/семантикой: полнотекст,
/// секция доков. Порядок и форматы — контракт парсинга фабричных агентов.
/// Голову печатает search_run ДО вызова — отказ полнотекстового фетча
/// (`?` здесь) выходит, когда голова уже в stdout (семантика мастера).
/// Публичный шов e2e (`cli/tests/search_e2e.rs`) — живое ядро гоняет
/// полный production-путь через него.
pub async fn search_lines(sy: &SiYuan, query: &str, limit: usize) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    lines.extend(fulltext_lines(&sy.search(query, limit).await?));
    let fetch = sy.doc_titles(DOC_TITLES_CAP).await;
    lines.extend(doc_section_result_lines(query, limit, fetch));
    Ok(lines)
}

pub(crate) async fn search_run(
    query: &str,
    limit: usize,
    threshold: f32,
    sofa: usize,
) -> Result<()> {
    let sy = SiYuan::from_env()?;
    // Голова — до полнотекстового фетча: отказный путь stdout байт-в-байт
    // прежний (голова + пустая строка, затем ошибка) — контракт парсинга.
    println!("{}\n", search_head(query));
    for line in search_lines(&sy, query, limit).await? {
        println!("{line}");
    }
    if sofa > 0 {
        print_sofa(query, sofa).await;
    }
    // Семантическая секция — always-on: отказы маршрута становятся
    // статусом внутри секции, команда не падает (exit 0).
    let cfg = crate::semantic::SemanticConfig::from_env();
    let section = crate::semantic::search_half(&sy, &cfg, query, limit, threshold).await;
    println!();
    for line in section {
        println!("{line}");
    }
    Ok(())
}

/// SOFA-блок полнотекстовой секции (opt-in): софт-деградация без кред —
/// предупреждение в stderr, локальный поиск не страдает.
async fn print_sofa(query: &str, limit: usize) {
    println!("\nSOFA (публичный):");
    match crate::sofa::search(query, limit).await {
        Ok(hits) if hits.is_empty() => println!("ничего не найдено"),
        Ok(hits) => {
            for (i, hit) in hits.iter().enumerate() {
                println!("{}. {} [{}]", i + 1, hit.title, hit.content_type);
                println!("    {}", hit.url);
            }
        }
        Err(e) => eprintln!("SOFA недоступен (продолжаю локально): {e:#}"),
    }
}

/// Обслуживание semantic-индекса: явный пересбор (громкие ошибки —
/// maintenance-команда) и статус.
pub(crate) async fn semantic_run(command: SemanticSub) -> Result<()> {
    let sy = SiYuan::from_env()?;
    let cfg = crate::semantic::SemanticConfig::from_env();
    match command {
        SemanticSub::Rebuild { resume } => {
            if resume {
                eprintln!(
                    "Продолжение semantic-индекса от watermark: {} через {}",
                    cfg.embed_model, cfg.embed_url
                );
            } else {
                eprintln!(
                    "Пересбор semantic-индекса: {} через {} (первичный билд ~55–70 мин)",
                    cfg.embed_model, cfg.embed_url
                );
            }
            let batches = std::sync::Mutex::new(0usize);
            let on_batch = std::sync::Arc::new(move |embedded: usize| {
                let mut count = batches.lock().expect("progress lock");
                *count += 1;
                if (*count).is_multiple_of(10) {
                    eprintln!("  чанков записано: {embedded}");
                }
            });
            let report = crate::semantic::rebuild(&sy, &cfg, Some(on_batch), resume).await?;
            println!(
                "Готово: {} чанков за {:.0} мин (пропущено коротких: {})",
                report.embedded,
                report.elapsed.as_secs_f64() / 60.0,
                report.skipped_short,
            );
        }
        SemanticSub::Status => {
            for line in crate::semantic::status(&sy, &cfg).await? {
                println!("{line}");
            }
        }
        SemanticSub::Sync => {
            for line in crate::semantic::sync_once(&sy, &cfg).await? {
                println!("{line}");
            }
        }
    }
    Ok(())
}
