//! Hub-источник semantic-индекса: full-скан uwuwu-hub (docs md через
//! git-клон + issues через gh API) → KernelRow. Дедуп — (updated, content):
//! updated для docs — хеш контента (файл не менялся → skip-unchanged), для
//! issues — GitHub updated_at (комменты двигают дату → переэмбедд).

use crate::semantic::store::ChunkMeta;
use crate::semantic::sync::KernelRow;
use crate::store::github_doc_store::GitHubDocStore;
use crate::store::github_gh::gh_json;
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// kernel-форма строки индекса (процесс_страница ждёт {id, content, ...}).
fn kernel_row(
    id: &str,
    root_id: &str,
    box_id: &str,
    hpath: &str,
    updated: &str,
    content: &str,
) -> Value {
    json!({
        "id": id, "root_id": root_id, "box": box_id,
        "hpath": hpath, "updated": updated, "content": content,
    })
}

pub(crate) const BOX_DOCS: &str = "hub-docs";
pub(crate) const BOX_ISSUES: &str = "hub-issues";

/// Полный скан контента hub: docs/**.md из клона + все issues репо.
pub(crate) async fn hub_rows(
    doc_store: &GitHubDocStore,
) -> Result<(Vec<Value>, HashMap<String, KernelRow>)> {
    let mut rows = docs_rows(doc_store).await?;
    rows.extend(issues_rows(&doc_store.repo).await?);
    let by_id: HashMap<String, KernelRow> = rows
        .iter()
        .filter_map(|r| {
            r["id"].as_str().map(|id| {
                (
                    id.to_string(),
                    KernelRow {
                        meta: ChunkMeta {
                            block_id: id.to_string(),
                            root_id: r["root_id"].as_str().unwrap_or("").to_string(),
                            box_id: r["box"].as_str().unwrap_or("").to_string(),
                            hpath: r["hpath"].as_str().unwrap_or("").to_string(),
                            updated: r["updated"].as_str().unwrap_or("").to_string(),
                        },
                        content: r["content"].as_str().unwrap_or("").to_string(),
                    },
                )
            })
        })
        .collect();
    Ok((rows, by_id))
}

/// docs/**.md клона: slug-путь = hpath, контент файла = чанк.
async fn docs_rows(doc_store: &GitHubDocStore) -> Result<Vec<Value>> {
    doc_store.ensure_clone().await?;
    let docs_root = doc_store.clone_path().join("docs");
    let mut rows = Vec::new();
    let mut stack = vec![docs_root.clone()];
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
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let content = String::from_utf8_lossy(&bytes).to_string();
            let rel = match path.strip_prefix(&docs_root) {
                Ok(p) => p.to_string_lossy().to_string(),
                Err(_) => continue,
            };
            let slug = rel.trim_end_matches(".md").to_string();
            rows.push(kernel_row(
                &format!("doc:{slug}"),
                &slug,
                BOX_DOCS,
                &slug,
                &content_hash(&content),
                &content,
            ));
        }
    }
    Ok(rows)
}

/// Issues репо (state=all, пагинация): id = issue:<number>,
/// hpath = «Задачи <label-proj>/<title>», content = title + body.
async fn issues_rows(repo: &str) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    let mut page = 1;
    loop {
        let data = gh_json(
            &[
                "api",
                "-X",
                "GET",
                &format!("repos/{repo}/issues"),
                "-F",
                "state=all",
                "-F",
                "per_page=100",
                "-F",
                &format!("page={page}"),
            ],
            None,
        )
        .await?;
        let Value::Array(items) = &data else {
            break;
        };
        let fetched = items.len();
        for issue in items {
            if issue.get("pull_request").is_some() {
                continue;
            }
            let number = issue["number"].to_string();
            let title = issue["title"].as_str().unwrap_or("").to_string();
            let body = issue["body"].as_str().unwrap_or("");
            let project = issue["labels"]
                .as_array()
                .and_then(|ls| {
                    ls.iter()
                        .find_map(|l| l["name"].as_str().and_then(|n| n.strip_prefix("proj:")))
                        .map(String::from)
                })
                .unwrap_or_else(|| "misc".into());
            let updated = issue["updated_at"].as_str().unwrap_or("").to_string();
            rows.push(kernel_row(
                &format!("issue:{number}"),
                &number,
                BOX_ISSUES,
                &format!("Задачи {project}/{title}"),
                &updated,
                &format!("# {title}\n\n{body}"),
            ));
        }
        if fetched < 100 {
            break;
        }
        page += 1;
    }
    Ok(rows)
}

/// Детерминенный хеш контента (SipHash с нулевым ключом — стабилен между
/// процессами): меняется только при изменении контента → skip-unchanged
/// дельты не переэмбеддит неизменные файлы.
fn content_hash(content: &str) -> String {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// block_id всех hub-строк, отсутствующих в свежем скане (файл снесён) —
/// на prune. Закрытые issues остаются (state=all их отдаёт).
pub(crate) fn stale_hub_ids(
    indexed_metas: &[crate::semantic::store::ChunkMeta],
    scanned: &[Value],
) -> Vec<String> {
    let scanned_ids: std::collections::HashSet<&str> =
        scanned.iter().filter_map(|r| r["id"].as_str()).collect();
    indexed_metas
        .iter()
        .filter(|m| m.box_id.starts_with("hub-") && !scanned_ids.contains(m.block_id.as_str()))
        .map(|m| m.block_id.clone())
        .collect()
}
