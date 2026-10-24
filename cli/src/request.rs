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

pub async fn render_queue(
    client: &mut crate::affine_client::AffineClient,
    cfg: &crate::affine_client::AffineConfig,
    command: Option<RequestSub>,
    cache: &ContentCache,
) -> anyhow::Result<()> {
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
                // native properties first; legacy meta lines as fallback
                let props =
                    crate::affine_queries::read_doc_properties(client, cfg, &doc.doc_id).await;
                let get = |key: &str| {
                    props
                        .iter()
                        .find(|(n, _)| n == key)
                        .map(|(_, v)| v.clone())
                        .or_else(|| meta_field(&doc.markdown, key))
                        .unwrap_or_else(|| "-".into())
                };
                println!("    target: {}", get("target"));
                println!("    reason: {}", get("reason"));
                println!("    status: {}", get("status"));
            }
        }
        Some(RequestSub::Get { title }) => {
            let doc = requests
                .into_iter()
                .find(|(slug, _)| slug.as_str() == title.as_str())
                .map(|(_, d)| d);
            match doc {
                Some(doc) => {
                    let props =
                        crate::affine_queries::read_doc_properties(client, cfg, &doc.doc_id).await;
                    if !props.is_empty() {
                        for (name, value) in &props {
                            println!("{name}: {value}");
                        }
                        println!();
                    }
                    println!("{}", doc.markdown);
                }
                None => anyhow::bail!("request not found: '{title}'"),
            }
        }
        Some(RequestSub::Delete { .. }) => {
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
