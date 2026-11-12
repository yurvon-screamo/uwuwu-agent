//! Полнотекст и заголовочный поиск GitHub DocStore: grep/rg по локальному
//! клону uwuwu-hub (search API кода не покрывает md-полнотекст стабильно и
//! лагает — клон консистентен, вердикт ревью о eventual consistency).
//! Заголовки: hpath-матч по путям docs/ (case-insensitive, AND-токены).

use super::doc_store::{FulltextHit, TitleHit};
use anyhow::Result;
use std::path::Path;

/// Полнотекст: rg если есть, иначе grep -rn -F. Сниппеты + имена файлов.
pub(super) async fn fulltext(
    clone_dir: &str,
    query: &str,
    top_n: usize,
) -> Result<Vec<FulltextHit>> {
    let docs = Path::new(clone_dir).join("docs");
    if !docs.exists() {
        return Ok(Vec::new());
    }
    let needle = query.trim();
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let mut hits = match has_ripgrep().await {
        true => rg_hits(&docs, needle).await?,
        false => grep_hits(&docs, needle).await?,
    };
    hits.truncate(top_n);
    Ok(hits)
}

async fn has_ripgrep() -> bool {
    tokio::process::Command::new("rg")
        .arg("--version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn rg_hits(docs: &Path, needle: &str) -> Result<Vec<FulltextHit>> {
    let out = tokio::process::Command::new("rg")
        .args(["-i", "--fixed-strings", "--max-count", "1", "-n"])
        .arg(needle)
        .arg(docs)
        .output()
        .await?;
    Ok(parse_line_pairs(&String::from_utf8_lossy(&out.stdout)))
}

async fn grep_hits(docs: &Path, needle: &str) -> Result<Vec<FulltextHit>> {
    let out = tokio::process::Command::new("grep")
        .args(["-rni", "-F", "-m", "1"])
        .arg(needle)
        .arg(docs)
        .output()
        .await?;
    Ok(parse_line_pairs(&String::from_utf8_lossy(&out.stdout)))
}

/// Строки «path:line:text» → хиты (slug — путь без docs/ и .md).
pub(crate) fn parse_line_pairs(stdout: &str) -> Vec<FulltextHit> {
    stdout
        .lines()
        .filter_map(|line| {
            let (path, rest) = line.split_once(':')?;
            let text = rest.split_once(':')?.1;
            let title = path.rsplit('/').next()?.trim_end_matches(".md").to_string();
            Some(FulltextHit {
                title,
                snippet: text.trim().to_string(),
                score: 1.0,
            })
        })
        .collect()
}

/// Заголовки: slug-пути, все токены запроса встречаются в пути
/// (case-insensitive, подстрочно); точная подстрока тайтла раньше токенных.
pub(crate) fn by_title(clone_dir: &str, query: &str, top_n: usize) -> Result<Vec<TitleHit>> {
    let docs = Path::new(clone_dir).join("docs");
    if !docs.exists() {
        return Ok(Vec::new());
    }
    let tokens: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect();
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    let mut exact = Vec::new();
    let mut tokenized = Vec::new();
    let mut stack = vec![docs.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let rel = path
                .strip_prefix(&docs)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let slug = rel.trim_end_matches(".md").to_string();
            let lower = rel.to_lowercase();
            if lower.contains(&query.to_lowercase()) {
                let title = title_of(&slug);
                exact.push(TitleHit { slug, title });
            } else if tokens.iter().all(|t| lower.contains(t)) {
                let title = title_of(&slug);
                tokenized.push(TitleHit { slug, title });
            }
        }
    }
    exact.extend(tokenized);
    exact.truncate(top_n);
    Ok(exact)
}

fn title_of(slug: &str) -> String {
    slug.rsplit('/').next().unwrap_or(slug).to_string()
}

/// Тестовый доступ к синхронным операциям (pub-слой DocStore — async).
pub(super) mod test_support {
    use super::by_title;
    pub(super) use super::parse_line_pairs;

    pub(super) fn by_title_sync(
        clone_dir: &str,
        query: &str,
        top_n: usize,
    ) -> Vec<super::TitleHit> {
        by_title(clone_dir, query, top_n).expect("test fs ops")
    }
}

/// Синхронный доступ для тестов (DocStore-слой — async).
#[cfg(test)]
pub(crate) fn by_title_sync(clone_dir: &str, query: &str, top_n: usize) -> Vec<TitleHit> {
    by_title(clone_dir, query, top_n).expect("test fs ops")
}
