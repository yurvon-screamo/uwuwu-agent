//! `uwuwu-cli write` — publish an article or a project doc straight into
//! AFFiNE (stdin markdown → workspace document). The FS copy is not written:
//! the workspace is the only destination, mirroring the facts pipeline.

use std::io::Read;
use std::path::Path;

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::affine_client::{AffineClient, AffineConfig};
use crate::affine_migrate::ensure_container;
use crate::affine_store::{doc_id_from_response, MigrationMap};
use crate::affine_tables::{split_long_tables, AFFINE_TABLE_CHUNK_ROWS};

/// Container name for a slug: experience articles live under "Experience",
/// project docs under "Projects/<project>".
fn container_for(project: Option<&str>) -> String {
    match project {
        Some(proj) => format!("Projects/{proj}"),
        None => "Experience".to_string(),
    }
}

pub async fn run_write(
    root: &Path,
    cfg: &AffineConfig,
    slug: &str,
    project: Option<&str>,
    title: &str,
    markdown: &str,
) -> Result<String> {
    crate::path::validate_slug(slug)?;
    if let Some(proj) = project {
        crate::path::sanitize_project(proj)?;
    }
    let mut client = AffineClient::connect(cfg).await?;
    let mut map = MigrationMap::load_for(root, cfg)?;
    if map.doc_id(slug).is_some() {
        anyhow::bail!("'{slug}' already exists in the workspace (affine map)");
    }
    let container_name = container_for(project);
    let intro = match project {
        Some(proj) => format!("# {proj}\n\nРанбуки, топология и креды проекта."),
        None => "# Experience\n\nСтатьи опыта uwuwu-базы знаний.".to_string(),
    };
    let container = ensure_container(
        &mut client,
        &mut map,
        &cfg.workspace,
        &container_name,
        &intro,
    )
    .await?;
    let resp = client
        .call_tool(
            "create_doc_from_markdown",
            json!({
                "workspaceId": cfg.workspace,
                "title": title,
                "markdown": split_long_tables(markdown, AFFINE_TABLE_CHUNK_ROWS),
                "parentDocId": container,
            }),
        )
        .await?;
    let doc_id = doc_id_from_response(&resp)
        .ok_or_else(|| anyhow!("created but no docId returned: {resp:.300}"))?;
    map.put_doc(slug, &doc_id);
    map.save()?;
    Ok(doc_id)
}

/// Read the markdown body from stdin.
pub fn read_markdown_stdin() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    if buf.trim().is_empty() {
        anyhow::bail!("пустой stdin: передайте markdown через пайп");
    }
    Ok(buf)
}
