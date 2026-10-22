//! FS → AFFiNE workspace migration orchestration: experience articles and
//! project docs as nested documents, facts as daily journal buckets via
//! affine_publish. Idempotent via MigrationMap — rerun resumes and skips;
//! the map is checkpointed after every migration unit.
//!
//! Slightly over the 200-line module limit: the three kind-specific flows
//! share the client/map/report plumbing end-to-end; splitting per kind would
//! triple that boilerplate without isolating any independent concept.

use std::path::Path;

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::affine_client::{AffineClient, AffineConfig};
use crate::affine_collect::{collect_experience, collect_facts, collect_projects, group_by_date};
use crate::affine_publish::upsert_daily_bucket;
use crate::affine_store::{
    doc_id_from_response, DocToMigrate, MigrationMap, AFFINE_DOC_TITLE_MAX_CHARS,
};
use crate::affine_tables::{split_long_tables, AFFINE_TABLE_CHUNK_ROWS};
use crate::facts::LockGuard;

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
/// Holds the facts pipeline lock for the whole run: both write affine_map.json.
pub async fn run_migration(
    root: &Path,
    kinds: &[Kind],
    cfg: &AffineConfig,
) -> Result<MigrationReport> {
    let _guard = LockGuard::acquire()?;
    let mut client = AffineClient::connect(cfg).await?;
    let mut map = MigrationMap::load(root)?;
    let mut report = MigrationReport::default();

    if kinds.contains(&Kind::Experience) {
        migrate_experience(&mut client, &mut map, &mut report, root, cfg).await?;
    }
    if kinds.contains(&Kind::Projects) {
        migrate_projects(&mut client, &mut map, &mut report, root, cfg).await?;
    }
    if kinds.contains(&Kind::Facts) {
        migrate_facts(&mut client, &mut map, &mut report, root, cfg).await?;
    }
    map.save()?;
    Ok(report)
}

async fn migrate_experience(
    client: &mut AffineClient,
    map: &mut MigrationMap,
    report: &mut MigrationReport,
    root: &Path,
    cfg: &AffineConfig,
) -> Result<()> {
    let container = ensure_container(
        client,
        map,
        &cfg.workspace,
        "Experience",
        "# Experience\n\nСтатьи опыта uwuwu-базы знаний.",
    )
    .await?;
    let collected = collect_experience(root);
    report.errors.extend(collected.errors.clone());
    for error in &collected.errors {
        eprintln!("warn: read error: {error}");
    }
    push_docs(
        client,
        map,
        report,
        &cfg.workspace,
        &container,
        collected.items,
    )
    .await
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
    let collected = collect_projects(root);
    report.errors.extend(collected.errors.clone());
    for error in &collected.errors {
        eprintln!("warn: read error: {error}");
    }
    for project in collected.items {
        let container = ensure_container(
            client,
            map,
            &cfg.workspace,
            &format!("Projects/{}", project.name),
            &project.readme,
        )
        .await?;
        let moved = client
            .call_tool(
                "move_doc",
                json!({"workspaceId": cfg.workspace, "docId": container, "toParentDocId": parent}),
            )
            .await;
        if let Err(e) = moved {
            report.note_error(&format!("nest project {}", project.name), e);
        }
        push_docs(
            client,
            map,
            report,
            &cfg.workspace,
            &container,
            project.docs,
        )
        .await?;
    }
    Ok(())
}

async fn migrate_facts(
    client: &mut AffineClient,
    map: &mut MigrationMap,
    report: &mut MigrationReport,
    root: &Path,
    cfg: &AffineConfig,
) -> Result<()> {
    let collected = collect_facts(root);
    report.errors.extend(collected.errors.clone());
    for error in &collected.errors {
        eprintln!("warn: read error: {error}");
    }
    for (date, day_facts) in group_by_date(&collected.items) {
        let all_mapped = day_facts.iter().all(|f| map.doc_id(&f.slug).is_some());
        if all_mapped {
            report.skipped += day_facts.len();
            continue;
        }
        // checkpoint after every day: a crash must not duplicate the days
        // that already landed in buckets
        match upsert_daily_bucket(client, map, &cfg.workspace, &date, &day_facts).await {
            Ok(count) => report.created += count,
            Err(e) => {
                report.skipped += day_facts.len();
                report.note_error(&format!("daily bucket {date}"), e);
            }
        }
        map.save()?;
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
                    "markdown": split_long_tables(&doc.markdown, AFFINE_TABLE_CHUNK_ROWS),
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
                        map.save()?;
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
                "markdown": split_long_tables(markdown, AFFINE_TABLE_CHUNK_ROWS)
            }),
        )
        .await?;
    let id = doc_id_from_response(&resp)
        .ok_or_else(|| anyhow!("container {name} created but no docId returned"))?;
    map.put_container(name, &id);
    Ok(id)
}

fn truncate_title(title: &str) -> String {
    title.chars().take(AFFINE_DOC_TITLE_MAX_CHARS).collect()
}
