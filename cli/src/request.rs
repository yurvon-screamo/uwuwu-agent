//! Request queue rendering over the AFFINE "Requests" container.
//! Queue creation lives in affine_queries::create_request; this module owns
//! the CLI surface: list / get / delete.

use clap::Subcommand;

use crate::content_cache::{ContentCache, DocKind};

#[derive(Subcommand)]
pub enum RequestSub {
    /// List queued requests.
    List,
    /// Show one request body by its title.
    Get { title: String },
    /// Delete a processed request by its title.
    Delete { title: String },
}

pub fn render_queue(command: Option<RequestSub>, cache: &ContentCache) -> anyhow::Result<()> {
    let requests = cache.docs_by_kind(&DocKind::Request);
    match command {
        None | Some(RequestSub::List) => {
            if requests.is_empty() {
                println!("Queue is empty.");
                return Ok(());
            }
            println!("{} request(s):", requests.len());
            for (slug, doc) in requests {
                println!("  {slug}");
                let target = meta_field(&doc.markdown, "target").unwrap_or_else(|| "-".into());
                let reason = meta_field(&doc.markdown, "reason").unwrap_or_else(|| "-".into());
                println!("    target: {target}");
                println!("    reason: {reason}");
            }
        }
        Some(RequestSub::Get { title }) => {
            let doc = requests
                .into_iter()
                .find(|(slug, _)| slug.as_str() == title.as_str())
                .map(|(_, d)| d);
            match doc {
                Some(doc) => println!("{}", doc.markdown),
                None => anyhow::bail!("request not found: '{title}'"),
            }
        }
        Some(RequestSub::Delete { .. }) => {
            // delete needs the network; the caller (lib) has the config —
            // rendered here as guidance, actual deletion happens in lib
            println!("delete: run `uwuwu-cli request delete <title>` from the command root");
        }
    }
    Ok(())
}

fn meta_field(markdown: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}: ");
    markdown
        .lines()
        .find(|l| l.starts_with(&prefix))
        .map(|l| l[prefix.len()..].trim().to_string())
}
