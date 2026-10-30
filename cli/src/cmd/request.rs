//! `uwuwu-cli request`: AFFiNE «Requests» queue — quick create or subcommands.

use std::path::PathBuf;

use crate::affine_client::AffineConfig;
use crate::affine_queries;
use crate::{facts, request};

pub(crate) struct RequestCmd {
    pub command: Option<request::RequestSub>,
    pub text: Vec<String>,
    pub stdin: bool,
    pub action: String,
    pub target: Option<String>,
    pub reason: Option<String>,
    pub attach: Vec<PathBuf>,
}

pub(crate) async fn run(
    cfg: &AffineConfig,
    root: &std::path::Path,
    cmd: RequestCmd,
) -> anyhow::Result<()> {
    let quick = quick_create_input(&cmd)?;
    match quick {
        Some(quick) => {
            let body = facts::read_text(quick.stdin, quick.text)?;
            let doc_id = affine_queries::create_request(
                root,
                cfg,
                quick.action,
                quick.target,
                quick.reason,
                &body,
            )
            .await?;
            println!("request created: {doc_id}");
            if !cmd.attach.is_empty() {
                crate::affine_blob::attach_files(cfg, &doc_id, &cmd.attach).await?;
            }
        }
        None => match cmd.command {
            Some(request::RequestSub::Delete { title }) => {
                affine_queries::delete_request(root, cfg, &title).await?;
                println!("request deleted: {title}");
            }
            other => {
                let (mut client, cache, _) = affine_queries::session(root, cfg).await?;
                request::render_queue(&mut client, cfg, other, &cache).await?;
            }
        },
    }
    Ok(())
}

struct QuickCreate<'a> {
    text: &'a [String],
    stdin: bool,
    action: &'a str,
    target: &'a str,
    reason: &'a str,
}

/// A bare call with text/--stdin is a quick create; it requires --target
/// and --reason. Flags without a body are a usage error.
fn quick_create_input(cmd: &RequestCmd) -> anyhow::Result<Option<QuickCreate<'_>>> {
    if cmd.command.is_none() && (!cmd.text.is_empty() || cmd.stdin) {
        let (target, reason) = (cmd.target.as_deref(), cmd.reason.as_deref());
        if target.is_none() || reason.is_none() {
            anyhow::bail!(
                "быстрое создание заявки требует --target <path> и --reason \"...\" \
                 (текст — аргументом или --stdin); очередь: uwuwu-cli request list"
            );
        }
        Ok(Some(QuickCreate {
            text: &cmd.text,
            stdin: cmd.stdin,
            action: &cmd.action,
            target: target.unwrap(),
            reason: reason.unwrap(),
        }))
    } else if cmd.command.is_none() && (cmd.target.is_some() || cmd.reason.is_some()) {
        anyhow::bail!(
            "указаны --target/--reason без тела заявки: передайте текст аргументом \
             или --stdin; просмотр очереди — без флагов"
        );
    } else {
        Ok(None)
    }
}
