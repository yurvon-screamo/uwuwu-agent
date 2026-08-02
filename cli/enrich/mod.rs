use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::chunk::{self, split_frontmatter};

use ollama::{build_client, ensure_model_available, summarize, MODEL_ID};

mod ollama;

const MAX_BODY_CHARS: usize = 2048;

pub enum EnrichFileOutcome {
    Empty,
    Skipped,
    Enriched { title: String, description: String },
}

pub struct EnrichDetail {
    pub file_name: String,
    pub outcome: EnrichFileOutcome,
}

pub struct EnrichSummary {
    pub total: usize,
    pub enriched: usize,
    pub details: Vec<EnrichDetail>,
}

fn collect_files(target: &Path) -> Result<Vec<PathBuf>> {
    if target.is_file() {
        return Ok(vec![target.to_path_buf()]);
    }
    if target.is_dir() {
        let mut files = collect_md_files(target);
        files.sort();
        return Ok(files);
    }
    anyhow::bail!("not found: {}", target.display())
}

fn collect_md_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(collect_md_files(&path));
            } else if path.extension().map(|e| e == "md").unwrap_or(false) {
                files.push(path);
            }
        }
    }
    files
}

async fn enrich_file(client: &reqwest::Client, path: &Path, dry_run: bool) -> Result<EnrichDetail> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let raw_bytes =
        std::fs::read(path).with_context(|| format!("cannot read: {}", path.display()))?;
    let line_ending = if raw_bytes.contains(&b'\r') {
        "\r\n"
    } else {
        "\n"
    };
    let content = String::from_utf8_lossy(&raw_bytes).replace("\r\n", "\n");

    let (frontmatter, body) = split_frontmatter(&content);
    let truncated: String = body.trim().chars().take(MAX_BODY_CHARS).collect();

    if truncated.is_empty() {
        return Ok(EnrichDetail {
            file_name,
            outcome: EnrichFileOutcome::Empty,
        });
    }

    let summary = summarize(client, &truncated).await?;
    let title = summary.title.trim().to_string();
    let description = summary.sub_title.trim().to_string();

    if title.is_empty() {
        return Ok(EnrichDetail {
            file_name,
            outcome: EnrichFileOutcome::Skipped,
        });
    }

    if !dry_run {
        let updated = if frontmatter.trim().is_empty() {
            let new_fm = format!("---\ntitle: {title}\ndescription: {description}\n---\n");
            format!("{new_fm}{body}")
        } else {
            chunk::update_frontmatter_fields(
                &content,
                &[("title", &title), ("description", &description)],
            )
        };
        let preserved = updated.replace('\n', line_ending);
        std::fs::write(path, preserved.as_bytes())
            .with_context(|| format!("cannot write: {}", path.display()))?;
    }

    Ok(EnrichDetail {
        file_name,
        outcome: EnrichFileOutcome::Enriched { title, description },
    })
}

pub async fn run_enrich(target: &Path, dry_run: bool) -> Result<EnrichSummary> {
    let client = build_client()?;
    ensure_model_available(&client).await?;

    let files = collect_files(target)?;
    let mut details = Vec::with_capacity(files.len());
    let mut enriched = 0;

    for file in &files {
        let detail = enrich_file(&client, file, dry_run).await?;
        if matches!(detail.outcome, EnrichFileOutcome::Enriched { .. }) {
            enriched += 1;
        }
        details.push(detail);
    }

    Ok(EnrichSummary {
        total: files.len(),
        enriched,
        details,
    })
}

pub async fn run(target: &Path, dry_run: bool) -> Result<()> {
    println!("Checking Ollama model {MODEL_ID}...");
    let summary = run_enrich(target, dry_run).await?;
    println!("Processing {} files...\n", summary.total);

    for d in &summary.details {
        match &d.outcome {
            EnrichFileOutcome::Empty => {}
            EnrichFileOutcome::Skipped => println!("  SKIP (no title): {}", d.file_name),
            EnrichFileOutcome::Enriched { title, description } => {
                if dry_run {
                    println!("  {}", d.file_name);
                    println!("    title: {title}");
                    println!("    description: {description}\n");
                } else {
                    println!("  OK: {}", d.file_name);
                }
            }
        }
    }

    println!("\nDone: {}/{} files", summary.enriched, summary.total);
    Ok(())
}

pub async fn enrich_document(path: &Path) -> Result<bool> {
    let client = build_client()?;
    ensure_model_available(&client).await?;
    let detail = enrich_file(&client, path, false).await?;
    Ok(matches!(detail.outcome, EnrichFileOutcome::Enriched { .. }))
}
