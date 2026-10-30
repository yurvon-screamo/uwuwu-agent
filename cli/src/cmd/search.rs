//! Search commands: unified semantic search and content-cache grep.

use crate::affine_client::AffineConfig;
use crate::affine_queries;
use crate::content_cache::DocKind;

pub(crate) struct SearchCmd {
    pub query: String,
    pub top: usize,
    pub tasks: usize,
    pub facts: usize,
    pub access: usize,
    pub sofa: usize,
    pub threshold: f32,
}

pub(crate) async fn run(
    cfg: &AffineConfig,
    root: &std::path::Path,
    cmd: SearchCmd,
) -> anyhow::Result<()> {
    let SearchCmd {
        query,
        top,
        tasks,
        facts,
        access,
        sofa,
        threshold,
    } = cmd;
    if !(0.0..=1.0).contains(&threshold) {
        anyhow::bail!("--threshold должен быть в 0..1, получено: {threshold}");
    }
    println!("Searching for: \"{query}\"\n");
    let sofa_fut = if sofa > 0 {
        Some(crate::sofa::search(&query, sofa))
    } else {
        None
    };
    // Server-side full-text search first (fresh index, no local deps);
    // fall back to local semantic search when unavailable or useless.
    let server_res = server_search(cfg, &query, top.max(access).max(10)).await;
    let printed = print_server_hits(root, &server_res);
    let use_local = !printed;
    let local_fut = async {
        if !use_local || top + facts + access == 0 {
            return Ok(Vec::new());
        }
        // тела задач/кронов — кэш с TTL (живой проход не чаще 5 мин);
        // шлюз недоступен → деградация на кэш, поиск не падает
        let extra = match crate::affine_client::AffineClient::connect(cfg).await {
            Ok(mut client) => affine_queries::tagged_docs_cached(&mut client, cfg, root).await,
            Err(e) => {
                eprintln!("affine: шлюз недоступен ({e:#}) — семантика задач из кэша");
                affine_queries::tagged_docs_cache_load(root)
            }
        };
        eprintln!("affine: тел задач/кронов в семантике: {}", extra.len());
        // search НЕ гонит синки (list_workspace_tree на 946 доках
        // упирается в лимит шлюза и сжирает минуту): локальные кэши
        // + tagged-кэш TTL 5 мин. Свежесть дают остальные cli-команды.
        let cache = crate::content_cache::ContentCache::load(root).unwrap_or_default();
        let index = crate::fact_index::FactIndex::load(root).unwrap_or_default();
        crate::affine_search::search(
            root,
            &cache,
            &index,
            &query,
            &crate::affine_search::SearchOpts {
                experience: top,
                facts,
                access,
                tasks,
                threshold,
            },
            &extra,
        )
        .await
    };
    let (local_res, sofa_res) = match sofa_fut {
        Some(fut) => {
            let (a, c) = tokio::join!(local_fut, fut);
            (a, Some(c))
        }
        None => (local_fut.await, None),
    };
    let hits = local_res?;
    for hit in &hits {
        let kind = match hit.kind {
            DocKind::Daily => "fact",
            DocKind::Project => "access",
            DocKind::Task => "task",
            DocKind::Cron => "cron",
            _ => "experience",
        };
        println!("{:.3}\t{kind}\t{}\t{}", hit.score, hit.title, hit.slug);
        if kind == "fact" {
            for line in hit.body.lines() {
                println!("    {line}");
            }
        }
    }
    let sofa_hits = sofa_res
        .map(|r| {
            r.unwrap_or_else(|err| {
                eprintln!("warning: SOFA недоступен ({err:#}); локальные результаты выше");
                Vec::new()
            })
        })
        .unwrap_or_default();
    if !sofa_hits.is_empty() {
        println!("\nPublic (Stack Overflow for Agents):");
        for h in sofa_hits {
            println!("  [{}] {}", h.content_type, h.title);
            println!("    {} (id {})", h.url, h.id);
        }
    }
    Ok(())
}

/// Server-side fulltext: `Result<Option<Vec<Hit>>, Error>` — `None` when
/// the server branch is unavailable, `Err` when it was tried and failed.
async fn server_search(
    cfg: &AffineConfig,
    query: &str,
    limit: usize,
) -> Option<Result<Vec<crate::affine_search_server::ServerHit>, anyhow::Error>> {
    match crate::affine_search_server::ServerSearchConfig::from_env() {
        Some(ss) => {
            match crate::affine_search_server::ServerSearch::connect(&ss, &cfg.workspace).await {
                Ok(conn) => Some(conn.search(query, limit).await),
                Err(e) => {
                    eprintln!("affine: серверный поиск недоступен ({e:#}); локальный ниже");
                    None
                }
            }
        }
        None => None,
    }
}

/// Print server hits mapped to cache slugs; returns whether anything was
/// printed (local search is skipped then).
fn print_server_hits(
    root: &std::path::Path,
    server_res: &Option<Result<Vec<crate::affine_search_server::ServerHit>, anyhow::Error>>,
) -> bool {
    let mut printed = 0usize;
    if let Some(Ok(hits)) = server_res {
        // doc_id→slug mapping from the LOCAL cache file only — the
        // server branch must not depend on the MCP gateway. Known
        // limitation: docs created from another machine after the
        // last sync are filtered out as "unknown" until the cache
        // catches up (freshness degrades to the cache age).
        let cache = crate::content_cache::ContentCache::load(root).unwrap_or_default();
        let by_doc_id: std::collections::BTreeMap<&str, (&str, &str, &str)> = cache
            .iter()
            .map(|(slug, doc)| {
                let kind = match doc.kind {
                    DocKind::Experience => "experience",
                    DocKind::Project => "access",
                    DocKind::Request => "request",
                    DocKind::Daily => "fact",
                    DocKind::Task => "task",
                    DocKind::Cron => "cron",
                    DocKind::Other => "other",
                };
                (
                    doc.doc_id.as_str(),
                    (slug.as_str(), kind, doc.title.as_str()),
                )
            })
            .collect();
        for h in hits {
            let Some((slug, kind, title)) = by_doc_id.get(h.doc_id.as_str()) else {
                continue; // daily buckets / requests / unknown — noise
            };
            let title = if h.title.is_empty() {
                *title
            } else {
                h.title.as_str()
            };
            println!("server\t{kind}\t{title}\t{slug}");
            if !h.snippet.is_empty() {
                println!("    {}", h.snippet.replace('\n', " "));
            }
            printed += 1;
        }
    }
    if let Some(Err(e)) = server_res {
        eprintln!("affine: серверный поиск упал ({e:#}); локальный ниже");
    }
    printed > 0
}

pub(crate) async fn grep(
    cfg: &AffineConfig,
    root: &std::path::Path,
    pattern: String,
) -> anyhow::Result<()> {
    let (_, cache, index) = affine_queries::session(root, cfg).await?;
    let hits = affine_queries::grep(&cache, &index, &pattern);
    if hits.is_empty() {
        println!("  No matches found.");
        return Ok(());
    }
    let mut last = String::new();
    for hit in &hits {
        if hit.slug != last {
            println!("\n{}", hit.slug);
            last = hit.slug.clone();
        }
        println!("  {}: {}", hit.line_number, hit.line);
    }
    println!("\n{} matches", hits.len());
    Ok(())
}
