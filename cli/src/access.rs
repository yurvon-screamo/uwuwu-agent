use std::io::IsTerminal;
use std::path::Path;

use anyhow::Result;
use clap::Subcommand;
use comfy_table::{presets::UTF8_FULL, Attribute, Cell, Color, ContentArrangement, Table};

use crate::grep;
use crate::path as safe_path;
use crate::search;

#[derive(Subcommand)]
pub enum AccessSub {
    /// Semantic search across project access docs via embeddings.
    Search {
        #[arg(help = "Project name")]
        project: String,
        #[arg(help = "Search query")]
        query: String,
        #[arg(
            short,
            long,
            default_value = "3",
            help = "Number of top results to return"
        )]
        top: usize,
    },
    /// Substring search across project access docs.
    Grep {
        #[arg(help = "Project name")]
        project: String,
        #[arg(help = "Search pattern (literal substring)")]
        pattern: String,
    },
    /// Read an access doc by slug.
    Get {
        #[arg(help = "Project name")]
        project: String,
        #[arg(help = "Access doc slug (filename without .md)")]
        slug: String,
    },
}

pub async fn handle(command: AccessSub, wiki_root: &Path, cache_path: &Path) -> Result<()> {
    match command {
        AccessSub::Search {
            project,
            query,
            top,
        } => handle_search(&project, &query, top, wiki_root, cache_path).await,
        AccessSub::Grep { project, pattern } => handle_grep(&project, &pattern, wiki_root),
        AccessSub::Get { project, slug } => handle_get(&project, &slug, wiki_root),
    }
}

async fn handle_search(
    project: &str,
    query: &str,
    top: usize,
    wiki_root: &Path,
    cache_path: &Path,
) -> Result<()> {
    let doc_type = safe_path::project_subdir_doc_type(project, "access")?;
    println!("Searching access docs of '{project}' for: \"{query}\"\n");

    let results = search::search(query, &doc_type, wiki_root, cache_path, top).await?;

    if results.is_empty() {
        println!("No results found (threshold 0.3).");
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("Score"),
            Cell::new("Title").add_attribute(Attribute::Bold),
            Cell::new("Description"),
            Cell::new("Slug").fg(Color::Cyan),
        ]);

    if !std::io::stdout().is_terminal() {
        table.set_width(180);
    }

    for r in &results {
        let slug = safe_path::slug_from_relpath(&r.filepath);
        let title = r.title.as_deref().unwrap_or("(no title)");
        let description = r.description.as_deref().unwrap_or("");
        table.add_row(vec![
            Cell::new(format!("{:.3}", r.score)).fg(Color::DarkGrey),
            Cell::new(title),
            Cell::new(description),
            Cell::new(slug).fg(Color::Cyan),
        ]);
    }

    println!("{table}");
    Ok(())
}

fn handle_grep(project: &str, pattern: &str, wiki_root: &Path) -> Result<()> {
    let doc_type = safe_path::project_subdir_doc_type(project, "access")?;

    let matches = grep::grep(pattern, &doc_type, wiki_root)?;

    if matches.is_empty() {
        println!("  No matches found.");
        return Ok(());
    }

    let mut last_file = String::new();
    for m in &matches {
        let slug = safe_path::slug_from_relpath(&m.filepath);
        if slug != last_file {
            println!("\n{slug}");
            last_file = slug;
        }
        println!("  {}: {}", m.line_number, m.line);
    }
    println!(
        "\n{} matches in {} files",
        matches.len(),
        matches
            .iter()
            .map(|m| &m.filepath)
            .collect::<std::collections::HashSet<_>>()
            .len()
    );
    Ok(())
}

fn handle_get(project: &str, slug: &str, wiki_root: &Path) -> Result<()> {
    let canonical = safe_path::resolve_existing(project, "access", slug, wiki_root)?;
    let content = std::fs::read_to_string(&canonical)?;
    println!("{content}");
    Ok(())
}
