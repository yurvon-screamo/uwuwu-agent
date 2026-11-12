//! kernel: живые пробы ядра (one-shot, без ретраев).

use crate::cmd::sy_dispatch::read::ls_lines;
use crate::cmd::cli::KernelSub;
use crate::siyuan::{FileFetch, SiYuan};
use anyhow::{bail, Context, Result};
use serde_json::json;

pub(super) async fn kernel_run(command: KernelSub, timeout_secs: u64) -> Result<()> {
    let sy = SiYuan::from_env_with_timeout(timeout_secs)?;
    match command {
        KernelSub::Ping => {
            sy.post_probe("/api/query/sql", json!({ "stmt": "select 1" }))
                .await?;
            println!("alive: sql ok");
        }
        KernelSub::Sql { stmt } => {
            let envelope = sy
                .post_probe("/api/query/sql", json!({ "stmt": stmt }))
                .await?;
            println!("{}", serde_json::to_string_pretty(&envelope)?);
        }
        KernelSub::File { path, out } => match sy.fetch_workspace_file(&path).await {
            FileFetch::Bytes(bytes) => {
                let saved = match out.as_deref() {
                    Some(out) => {
                        std::fs::write(out, &bytes)
                            .with_context(|| format!("не удалось записать {out}"))?;
                        format!(" → {out}")
                    }
                    None => String::new(),
                };
                println!("Bytes ({} bytes){saved}", bytes.len());
            }
            FileFetch::Missing => println!("Missing (kernel 404)"),
            FileFetch::Failed(err) => bail!("kernel file {path}: {err}"),
        },
        KernelSub::Ls { path } => {
            let envelope = sy
                .post_probe("/api/file/readDir", json!({ "path": path }))
                .await?;
            let entries = envelope
                .get("data")
                .and_then(|d| d.as_array())
                .cloned()
                .unwrap_or_default();
            for line in ls_lines(&entries) {
                println!("{line}");
            }
        }
        KernelSub::Export { doc_id } => {
            crate::siyuan_id::ensure_node_id(&doc_id, "doc_id")?;
            let envelope = sy
                .post_probe("/api/export/exportMdContent", json!({ "id": doc_id }))
                .await?;
            let content = envelope
                .pointer("/data/content")
                .and_then(|c| c.as_str())
                .unwrap_or("");
            if content.is_empty() {
                eprintln!(
                    "(export пуст: 0 символов — док пуст, деградация индекса или неверный id)"
                );
            }
            print!("{content}");
        }
        KernelSub::Render { id, page } => {
            crate::siyuan_id::ensure_node_id(&id, "id")?;
            let (av_id, block_id) = crate::siyuan_task::resolve_av(&sy, &id).await?;
            let render = sy.db_render(&av_id, &block_id, page).await?;
            println!("{}", serde_json::to_string_pretty(&render)?);
        }
    }
    Ok(())
}
