use std::io::IsTerminal;

use anyhow::Result;
use comfy_table::{presets::UTF8_FULL, Attribute, Cell, Color, ContentArrangement, Table};

use crate::path as safe_path;
use crate::search;
use crate::task_query::TaskQuery;

pub struct SearchArgs {
    pub project: String,
    pub query: String,
    pub top: usize,
    pub status: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub priority: Option<String>,
    pub deadline_from: Option<String>,
    pub deadline_to: Option<String>,
    pub designed: Option<bool>,
}

pub async fn run_search(
    args: &SearchArgs,
    wiki_root: &std::path::Path,
    cache_path: &std::path::Path,
) -> Result<Vec<search::SearchResult>> {
    if let (Some(f), Some(t)) = (&args.from, &args.to) {
        if f > t {
            anyhow::bail!("invalid date range: --from {f} is after --to {t}");
        }
    }
    if let (Some(f), Some(t)) = (&args.deadline_from, &args.deadline_to) {
        if f > t {
            anyhow::bail!("invalid deadline range: --deadline-from {f} is after --deadline-to {t}");
        }
    }

    let doc_type = safe_path::project_subdir_doc_type(&args.project, "tasks")?;

    let filter = TaskQuery {
        status: args.status.clone(),
        from: args.from.clone(),
        to: args.to.clone(),
        priority: args.priority.clone(),
        deadline_from: args.deadline_from.clone(),
        deadline_to: args.deadline_to.clone(),
        designed: args.designed,
    };
    let results = if filter.is_empty() {
        search::search(&args.query, &doc_type, wiki_root, cache_path, args.top).await?
    } else {
        search::search_with_filter(
            &args.query,
            &doc_type,
            wiki_root,
            cache_path,
            args.top,
            Some(&|p: &std::path::Path| filter.matches_strict(p)),
        )
        .await?
    };

    Ok(results)
}

pub async fn handle_search(
    args: &SearchArgs,
    wiki_root: &std::path::Path,
    cache_path: &std::path::Path,
) -> Result<()> {
    println!(
        "Searching tasks of '{}' for: \"{}\"\n",
        args.project, args.query
    );

    let results = run_search(args, wiki_root, cache_path).await?;

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
            Cell::new("Pr"),
            Cell::new("Dl"),
            Cell::new("Title").add_attribute(Attribute::Bold),
            Cell::new("Description"),
            Cell::new("Slug").fg(Color::Cyan),
        ]);

    if !std::io::stdout().is_terminal() {
        table.set_width(220);
    }

    for r in &results {
        let slug = safe_path::slug_from_relpath(&r.filepath);
        let title = r.title.as_deref().unwrap_or("(no title)");
        let description = r.description.as_deref().unwrap_or("");
        table.add_row(vec![
            Cell::new(format!("{:.3}", r.score)).fg(Color::DarkGrey),
            Cell::new(r.priority.as_deref().unwrap_or("")).fg(Color::Yellow),
            Cell::new(r.deadline.as_deref().unwrap_or("")).fg(Color::Magenta),
            Cell::new(title),
            Cell::new(description),
            Cell::new(slug).fg(Color::Cyan),
        ]);
    }

    println!("{table}");
    Ok(())
}

pub fn parse_date(s: &str) -> Result<String, String> {
    let b = s.as_bytes();
    let ok = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[0..4].iter().all(|x| x.is_ascii_digit())
        && b[5..7].iter().all(|x| x.is_ascii_digit())
        && b[8..10].iter().all(|x| x.is_ascii_digit());
    if ok {
        Ok(s.to_string())
    } else {
        Err(format!("invalid date format: {s}, expected YYYY-MM-DD"))
    }
}
