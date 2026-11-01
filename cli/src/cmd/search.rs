//! Search commands: server-side AFFiNE full-text search (Manticore) + SOFA.
//! Stateless — no local corpus, no semantic fallback; the gateway is the
//! single source of truth.

use crate::affine_client::AffineConfig;

pub(crate) struct SearchCmd {
    pub query: String,
    pub limit: usize,
    pub sofa: usize,
    pub threshold: f32,
}

pub(crate) async fn run(cfg: &AffineConfig, cmd: SearchCmd) -> anyhow::Result<()> {
    let SearchCmd {
        query,
        limit,
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

    let server_res = server_search(cfg, &query, limit.max(10)).await;
    let mut printed = 0usize;
    match server_res {
        Some(Ok(hits)) => {
            for h in &hits {
                println!(
                    "server\t{}",
                    if h.title.is_empty() { "-" } else { &h.title }
                );
                if !h.snippet.is_empty() {
                    println!("    {}", h.snippet.replace('\n', " "));
                }
                println!(
                    "    {}/workspace/{}/{}",
                    crate::ui_url(),
                    cfg.workspace,
                    h.doc_id
                );
                printed += 1;
            }
        }
        Some(Err(e)) => eprintln!("affine: серверный поиск упал ({e:#})"),
        None => {}
    }
    if printed == 0 {
        eprintln!("affine: серверный поиск недоступен/пуст — локального фоллбека больше нет");
        eprintln!("  (нужны UWUWU_AFFINE_UI_URL/_EMAIL/_PASSWORD; сервер — Manticore афины)");
    }

    let sofa_hits = match sofa_fut {
        Some(fut) => fut.await.unwrap_or_else(|err| {
            eprintln!("warning: SOFA недоступен ({err:#})");
            Vec::new()
        }),
        None => Vec::new(),
    };
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
                    eprintln!("affine: серверный поиск недоступен ({e:#})");
                    None
                }
            }
        }
        None => None,
    }
}
