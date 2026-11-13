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
    let needle = query.to_lowercase();
    if needle.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut exact = Vec::new();
    let mut tokenized = Vec::new();
    for slug in walk_md_slugs(&docs) {
        let lower = slug.to_lowercase();
        if lower.contains(&needle) {
            exact.push(hit(&slug));
        } else if matches_tokens(&lower, &needle) {
            tokenized.push(hit(&slug));
        }
    }
    exact.extend(tokenized);
    exact.truncate(top_n);
    Ok(exact)
}

/// AND-токены запроса по пути (whitespace-разбиение, подстрочно).
fn matches_tokens(lower_path: &str, needle: &str) -> bool {
    needle.split_whitespace().all(|t| lower_path.contains(t))
}

fn hit(slug: &str) -> TitleHit {
    let title = title_of(slug);
    TitleHit {
        slug: slug.to_string(),
        title,
    }
}

/// Все md-слаги под docs/ (итеративный обход, сортировка для детермизма).
fn walk_md_slugs(docs: &Path) -> Vec<String> {
    let mut slugs = Vec::new();
    let mut stack = vec![docs.to_path_buf()];
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
                .strip_prefix(docs)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            slugs.push(rel.trim_end_matches(".md").to_string());
        }
    }
    slugs.sort();
    slugs
}

fn title_of(slug: &str) -> String {
    slug.rsplit('/').next().unwrap_or(slug).to_string()
}

/// Синхронный доступ для тестов (DocStore-слой — async).
#[cfg(test)]
pub(crate) fn by_title_sync(clone_dir: &str, query: &str, top_n: usize) -> Vec<TitleHit> {
    by_title(clone_dir, query, top_n).expect("test fs ops")
}
