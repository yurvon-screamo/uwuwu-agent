//! FS → AFFiNE workspace migration: experience articles, project docs and
//! facts grouped into daily buckets (fact = entity, daily doc = bucket).
//! Idempotent via affine_store::MigrationMap — rerun resumes and skips.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::affine_client::{AffineClient, AffineConfig};
use crate::affine_store::{
    doc_id_from_response, render_fact_section, split_frontmatter, split_long_tables, DocToMigrate,
    FactEntry, MigrationMap,
};

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Experience,
    Projects,
    Facts,
}

#[derive(Default)]
pub struct MigrationReport {
    pub created: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

impl MigrationReport {
    fn note_error(&mut self, what: &str, err: anyhow::Error) {
        eprintln!("warn: {what}: {err:#}");
        self.errors.push(format!("{what}: {err:#}"));
    }
}

/// Migrate the requested kinds from the wiki root into the workspace.
pub async fn run_migration(
    root: &Path,
    kinds: &[Kind],
    cfg: &AffineConfig,
) -> Result<MigrationReport> {
    let mut client = AffineClient::connect(cfg).await?;
    let mut map = MigrationMap::load(root)?;
    let mut report = MigrationReport::default();

    if kinds.contains(&Kind::Experience) {
        let container = ensure_container(
            &mut client,
            &mut map,
            &cfg.workspace,
            "Experience",
            "# Experience\n\nСтатьи опыта uwuwu-базы знаний.",
        )
        .await?;
        let docs = collect_experience(root);
        push_docs(
            &mut client,
            &mut map,
            &mut report,
            &cfg.workspace,
            &container,
            docs,
        )
        .await?;
    }
    if kinds.contains(&Kind::Projects) {
        migrate_projects(&mut client, &mut map, &mut report, root, cfg).await?;
    }
    if kinds.contains(&Kind::Facts) {
        let facts = collect_facts(root);
        migrate_facts(&mut client, &mut map, &mut report, &cfg.workspace, &facts).await?;
    }
    map.save()?;
    Ok(report)
}

/// Publish freshly extracted facts of one day into its daily bucket.
/// Called from the facts pipeline (best-effort: caller decides on errors).
pub async fn publish_facts(root: &Path, cfg: &AffineConfig, facts: &[FactEntry]) -> Result<usize> {
    let mut client = AffineClient::connect(cfg).await?;
    let mut map = MigrationMap::load(root)?;
    let mut published = 0usize;
    for (date, day_facts) in group_by_date(facts) {
        published +=
            upsert_daily_bucket(&mut client, &mut map, &cfg.workspace, &date, &day_facts).await?;
    }
    map.save()?;
    Ok(published)
}

/// Create the day bucket with `facts` or append the ones not migrated yet.
/// Returns how many facts became visible in the bucket.
async fn upsert_daily_bucket(
    client: &mut AffineClient,
    map: &mut MigrationMap,
    workspace: &str,
    date: &str,
    day_facts: &[&FactEntry],
) -> Result<usize> {
    let fresh: Vec<&FactEntry> = day_facts
        .iter()
        .filter(|f| map.doc_id(&f.slug).is_none())
        .copied()
        .collect();
    if fresh.is_empty() {
        return Ok(0);
    }
    let markdown: String = fresh.iter().map(|f| render_fact_section(f)).collect();
    let bucket_id = match map.daily_bucket(date) {
        Some(id) => {
            client
                .call_tool(
                    "append_markdown",
                    json!({
                        "workspaceId": workspace, "docId": id, "markdown": markdown
                    }),
                )
                .await?;
            id.to_string()
        }
        None => {
            let resp = client
                .call_tool(
                    "create_doc_from_markdown",
                    json!({
                        "workspaceId": workspace, "title": date, "markdown": markdown
                    }),
                )
                .await?;
            let id = doc_id_from_response(&resp)
                .ok_or_else(|| anyhow!("daily bucket {date} created but no docId returned"))?;
            client
                .call_tool(
                    "set_doc_journal",
                    json!({
                        "workspaceId": workspace, "docId": id, "date": date
                    }),
                )
                .await?;
            map.put_daily_bucket(date, &id);
            id
        }
    };
    let count = fresh.len();
    // docId of a fact is its bucket; slugs stay addressable via the map
    for f in &fresh {
        map.put_doc(&f.slug, &bucket_id);
    }
    Ok(count)
}

async fn migrate_projects(
    client: &mut AffineClient,
    map: &mut MigrationMap,
    report: &mut MigrationReport,
    root: &Path,
    cfg: &AffineConfig,
) -> Result<()> {
    let parent = ensure_container(
        client,
        map,
        &cfg.workspace,
        "Projects",
        "# Projects\n\nРанбуки, топология и креды проектов.",
    )
    .await?;
    for (name, readme, docs) in collect_projects(root) {
        let container = ensure_container(
            client,
            map,
            &cfg.workspace,
            &format!("Projects/{name}"),
            &readme,
        )
        .await?;
        let moved = client
            .call_tool(
                "move_doc",
                json!({
                    "workspaceId": cfg.workspace, "docId": container, "toParentDocId": parent
                }),
            )
            .await;
        if let Err(e) = moved {
            report.note_error(&format!("nest project {name}"), e);
        }
        push_docs(client, map, report, &cfg.workspace, &container, docs).await?;
    }
    Ok(())
}

async fn migrate_facts(
    client: &mut AffineClient,
    map: &mut MigrationMap,
    report: &mut MigrationReport,
    workspace: &str,
    facts: &[FactEntry],
) -> Result<()> {
    for (date, day_facts) in group_by_date(facts) {
        let all_mapped = day_facts.iter().all(|f| map.doc_id(&f.slug).is_some());
        if all_mapped {
            report.skipped += day_facts.len();
            continue;
        }
        let res = upsert_daily_bucket(client, map, workspace, &date, &day_facts).await;
        match res {
            Ok(count) => report.created += count,
            Err(e) => {
                report.skipped += day_facts.len();
                report.note_error(&format!("daily bucket {date}"), e);
            }
        }
    }
    Ok(())
}

async fn push_docs(
    client: &mut AffineClient,
    map: &mut MigrationMap,
    report: &mut MigrationReport,
    workspace: &str,
    parent: &str,
    docs: Vec<DocToMigrate>,
) -> Result<()> {
    for doc in docs {
        if map.doc_id(&doc.slug).is_some() {
            report.skipped += 1;
            continue;
        }
        let res = client
            .call_tool(
                "create_doc_from_markdown",
                json!({
                    "workspaceId": workspace,
                    "title": truncate_title(&doc.title),
                    "markdown": split_long_tables(&doc.markdown, 18),
                    "parentDocId": parent,
                }),
            )
            .await;
        match res {
            Ok(resp) => match doc_id_from_response(&resp) {
                Some(id) => {
                    map.put_doc(&doc.slug, &id);
                    report.created += 1;
                    if report.created.is_multiple_of(25) {
                        eprintln!("  … created {}", report.created);
                        let _ = map.save();
                    }
                }
                None => report.note_error(
                    &format!("create {}", doc.slug),
                    anyhow!("response carries no docId: {resp:.300}"),
                ),
            },
            Err(e) => report.note_error(&format!("create {}", doc.slug), e),
        }
    }
    Ok(())
}

async fn ensure_container(
    client: &mut AffineClient,
    map: &mut MigrationMap,
    workspace: &str,
    name: &str,
    markdown: &str,
) -> Result<String> {
    if let Some(id) = map.container(name) {
        return Ok(id.to_string());
    }
    let title = name.rsplit('/').next().unwrap_or(name);
    let resp = client
        .call_tool(
            "create_doc_from_markdown",
            json!({
                "workspaceId": workspace, "title": title,
                "markdown": split_long_tables(markdown, 18)
            }),
        )
        .await?;
    let id = doc_id_from_response(&resp)
        .ok_or_else(|| anyhow!("container {name} created but no docId returned"))?;
    map.put_container(name, &id);
    Ok(id)
}

// ---------- FS collectors ----------

fn collect_md(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_md(dir, &mut out);
    out.sort();
    out
}

fn walk_md(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_md(&path, out);
        } else if path.extension().map(|x| x == "md").unwrap_or(false) {
            out.push(path);
        }
    }
}

fn collect_experience(root: &Path) -> Vec<DocToMigrate> {
    let base = root.join("experience");
    collect_md(&base)
        .into_iter()
        .map(|path| {
            let rel = path.strip_prefix(&base).unwrap_or(&path);
            let slug = rel.with_extension("").to_string_lossy().replace('\\', "/");
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let (fm, body) = split_frontmatter(&content);
            DocToMigrate {
                title: fm.title.unwrap_or_else(|| slug.clone()),
                markdown: body,
                slug,
            }
        })
        .collect()
}

fn collect_projects(root: &Path) -> Vec<(String, String, Vec<DocToMigrate>)> {
    let projects_root = root.join("projects");
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&projects_root) else {
        return out;
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let readme = std::fs::read_to_string(dir.join("README.md"))
            .map(|raw| split_frontmatter(&raw).1)
            .unwrap_or_else(|_| format!("# {name}"));
        let docs: Vec<DocToMigrate> = collect_md(&dir)
            .into_iter()
            .filter(|p| p.file_name().map(|n| n != "README.md").unwrap_or(false))
            .map(|path| {
                let slug = format!(
                    "{}/{}",
                    name,
                    path.file_stem().unwrap_or_default().to_string_lossy()
                );
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                let (fm, body) = split_frontmatter(&content);
                DocToMigrate {
                    title: fm.title.unwrap_or_else(|| slug.clone()),
                    markdown: body,
                    slug,
                }
            })
            .collect();
        out.push((name, readme, docs));
    }
    out
}

fn collect_facts(root: &Path) -> Vec<FactEntry> {
    collect_md(&root.join("facts"))
        .into_iter()
        .map(|path| {
            let slug = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let (fm, statement) = split_frontmatter(&content);
            FactEntry {
                date: fm.updated.clone().unwrap_or_else(|| month_fallback(&path)),
                title: fm.title.unwrap_or_else(|| slug.clone()),
                statement,
                tags: fm.tags,
                confidence: fm.confidence.unwrap_or_else(|| "-".into()),
                sources: fm.sources,
                slug,
            }
        })
        .collect()
}

fn month_fallback(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .map(|n| format!("{}-01", n.to_string_lossy()))
        .unwrap_or_else(|| "1970-01-01".into())
}

pub fn group_by_date(facts: &[FactEntry]) -> BTreeMap<String, Vec<&FactEntry>> {
    let mut groups: BTreeMap<String, Vec<&FactEntry>> = BTreeMap::new();
    for fact in facts {
        groups.entry(fact.date.clone()).or_default().push(fact);
    }
    groups
}

fn truncate_title(title: &str) -> String {
    title.chars().take(120).collect()
}
