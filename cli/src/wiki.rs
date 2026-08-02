use std::io::IsTerminal;
use std::path::Path;

use anyhow::Result;
use clap::Subcommand;
use comfy_table::{presets::UTF8_FULL, Attribute, Cell, Color, ContentArrangement, Table};

use crate::get;
use crate::grep;
use crate::path;
use crate::search;

#[derive(Subcommand)]
pub enum WikiSub {
    /// Semantic search over experience/.
    Search {
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
    /// Substring search over experience/.
    Grep {
        #[arg(help = "Search pattern (literal substring)")]
        pattern: String,
    },
    /// Read a document by slug (filename without .md, e.g. 'axum').
    Get {
        #[arg(help = "Document slug (filename without .md)")]
        slug: String,
    },
}

pub async fn handle(command: WikiSub, wiki_root: &Path, cache_path: &Path) -> Result<()> {
    match command {
        WikiSub::Search { query, top } => handle_search(&query, top, wiki_root, cache_path).await,
        WikiSub::Grep { pattern } => handle_grep(&pattern, wiki_root),
        WikiSub::Get { slug } => handle_get(&slug, wiki_root),
    }
}

async fn handle_search(query: &str, top: usize, wiki_root: &Path, cache_path: &Path) -> Result<()> {
    println!("Searching experience for: \"{query}\"\n");
    let results = search::search(query, "experience", wiki_root, cache_path, top).await?;
    if results.is_empty() {
        println!("  No results found (threshold 0.3).");
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
        let slug = path::slug_from_relpath(&r.filepath);
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

fn handle_grep(pattern: &str, wiki_root: &Path) -> Result<()> {
    let matches = grep::grep(pattern, "experience", wiki_root)?;
    if matches.is_empty() {
        println!("  No matches found.");
        return Ok(());
    }
    let mut last_file = String::new();
    for m in &matches {
        let slug = path::slug_from_relpath(&m.filepath);
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

fn handle_get(slug: &str, wiki_root: &Path) -> Result<()> {
    let content = get::get_document_by_slug(slug, wiki_root)?;
    println!("{content}");
    Ok(())
}
