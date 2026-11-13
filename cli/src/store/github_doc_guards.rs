//! Гварды входа write-path docs (задача 46): frontmatter-отказ,
//! title-H1 стрип, шапка create, ADR-номера, разбор слага. Текстовая
//! логика — отдельно от транзакционной записи (github_doc_write.rs).

use super::github_doc_store::GitHubDocStore;
use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Первая строка входа `---` — frontmatter-шапка; её ведёт docs create.
pub(crate) fn reject_frontmatter(content: &str) -> Result<()> {
    let first = content.lines().next().unwrap_or("").trim_end();
    if first == "---" {
        bail!("вход несёт frontmatter-шапку — её ведёт docs create, снеси шапку из входа");
    }
    Ok(())
}

/// Первая строка входа `# <тайтл>` срезается (паритет title-H1 стрипа
/// wiki_ops create): дубль шапки-заголовка — артефакт экспорта.
pub(crate) fn strip_title_h1<'a>(content: &'a str, title: &str) -> &'a str {
    let trimmed = content.trim_start_matches('\n');
    let first = trimmed.lines().next().unwrap_or("");
    if first.trim_end() == format!("# {title}") {
        trimmed[first.len()..].trim_start_matches('\n')
    } else {
        trimmed
    }
}

/// Шапка создаваемого дока: frontmatter (title/date/lastmod) + H1.
fn doc_header(title: &str) -> String {
    let now = chrono::Local::now().to_rfc3339();
    format!("---\ntitle: {title}\ndate: {now}\nlastmod: {now}\n---\n\n# {title}\n\n")
}

/// Номер из тайтла «ADR-NNN: …» (None — не ADR-тайтл). Ручной парс, без regex.
fn adr_number(title: &str) -> Option<String> {
    let rest = title.strip_prefix("ADR-")?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    (!digits.is_empty()).then_some(digits)
}

/// Разбор слага на (родительская папка, тайтл).
pub(crate) fn split_slug(slug: &str) -> (String, String) {
    match slug.trim_matches('/').rsplit_once('/') {
        Some((parent, title)) => (parent.to_string(), title.to_string()),
        None => (String::new(), slug.trim_matches('/').to_string()),
    }
}

/// warn-строки create: первая док новой папки (легитимна, стоп-крана нет —
/// ревью 12.11); занятый ADR-номер в родительской папке.
pub(crate) fn create_warnings(store: &GitHubDocStore, slug: &str) -> Vec<String> {
    let (parent, title) = split_slug(slug);
    let dir = store.clone_path().join("docs").join(&parent);
    let mut out = Vec::new();
    if !parent.is_empty() && !dir.is_dir() {
        out.push(format!("warn: папка «{parent}» новая — первый док ветки"));
        return out;
    }
    let Some(n) = adr_number(&title) else {
        return out;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let file_num = adr_number(name.trim_end_matches(".md"));
        if file_num.as_deref() == Some(n.as_str()) {
            out.push(format!(
                "warn: ADR-номер {n} занят в {parent} («{name}») — проверь нумерацию"
            ));
        }
    }
    out
}

/// Полное содержимое создаваемого дока: шапка + нормализованный вход.
pub(crate) fn create_bytes(title: &str, content: &str) -> Vec<u8> {
    let body = strip_title_h1(content, title);
    format!("{}{body}", doc_header(title)).into_bytes()
}
