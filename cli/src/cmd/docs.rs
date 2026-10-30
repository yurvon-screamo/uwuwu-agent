//! Data-access commands: read, blob, shot, projects, refresh.

use anyhow::Context as _;

use crate::affine_client::AffineConfig;
use crate::affine_queries;

pub(crate) async fn read(
    cfg: &AffineConfig,
    root: &std::path::Path,
    slug: String,
) -> anyhow::Result<()> {
    if let Some(post_id) = slug.strip_prefix("sofa/") {
        crate::wiki::sofa_post(post_id).await?
    } else {
        let content = affine_queries::read(root, cfg, &slug).await?;
        println!("{content}");
    }
    Ok(())
}

pub(crate) async fn blob(cfg: &AffineConfig, key: &str, out: Option<String>) -> anyhow::Result<()> {
    let ss = crate::affine_search_server::ServerSearchConfig::from_env()
        .context("нужны UWUWU_AFFINE_UI_URL/_EMAIL/_PASSWORD")?;
    let bytes = crate::affine_blob::download_blob(&ss, &cfg.workspace, key).await?;
    match out.as_deref() {
        Some("-") => {
            use std::io::Write;
            if unsafe { libc::isatty(1) } == 1 {
                eprintln!("warning: бинарные данные ({}) в терминал; перенаправь в файл: uwuwu-cli blob {key} --out файл", bytes.len());
            }
            std::io::stdout().write_all(&bytes)?;
        }
        Some(path) => {
            std::fs::write(path, &bytes)?;
            println!("{key} → {} bytes → {path}", bytes.len());
        }
        None => {
            let name = key.rsplit('/').next().unwrap_or("blob");
            std::fs::write(name, &bytes)?;
            println!("{key} → {} bytes → {}", bytes.len(), name);
        }
    }
    Ok(())
}

pub(crate) async fn shot(
    cfg: &AffineConfig,
    root: &std::path::Path,
    slug: String,
    out: Option<String>,
    edgeless: bool,
) -> anyhow::Result<()> {
    let ss = crate::affine_search_server::ServerSearchConfig::from_env()
        .context("нужны UWUWU_AFFINE_UI_URL/_EMAIL/_PASSWORD")?;
    let doc_id = affine_queries::resolve_doc_id(root, cfg, &slug)
        .await?
        .with_context(|| format!("док '{slug}' не найден"))?;
    let out = out.unwrap_or_else(|| format!("shot-{doc_id}.png"));
    let path = crate::affine_blob::shot_doc(
        &ss,
        &cfg.workspace,
        &doc_id,
        std::path::Path::new(&out),
        edgeless,
    )
    .await?;
    println!("скриншот: {path}");
    Ok(())
}

pub(crate) async fn projects(cfg: &AffineConfig, root: &std::path::Path) -> anyhow::Result<()> {
    let (_, cache, _) = affine_queries::session(root, cfg).await?;
    let rows = affine_queries::projects(&cache);
    if rows.is_empty() {
        println!("No projects found in the workspace.");
    }
    for row in rows {
        println!("{}\t{} docs\t{}", row.name, row.docs, row.description);
    }
    Ok(())
}

pub(crate) async fn refresh(cfg: &AffineConfig, root: &std::path::Path) -> anyhow::Result<()> {
    // явный refresh — сброс слепого-tree флага: пробуем полный проход
    crate::content_cache::tree_disabled_clear(root);
    let mut client = crate::affine_client::AffineClient::connect(cfg).await?;
    // full refetch: fresh cache replaces the incremental one
    let mut fresh = crate::content_cache::ContentCache::default();
    let report = fresh.sync(&mut client, cfg, root).await?;
    let mut index = crate::fact_index::FactIndex::load(root)?;
    affine_queries::sync_fact_buckets(&mut client, cfg, root, &mut index).await?;
    println!(
        "synced: {} docs fetched, {} facts in dedup index",
        report.fetched,
        index.iter().count()
    );
    Ok(())
}
