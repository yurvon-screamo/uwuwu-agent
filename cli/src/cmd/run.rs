//! CLI-маршрутизация hub-режима (задача
//! 20261112155302-ttyp35g): task/search/read/docs/semantic/projects/person/
//! cron — через uwuwu-hub (TaskStore/DocStore + gh). Gh-листинги
//! (projects/cron) — hub_lists (полная пагинация ADR-020).

use crate::cmd::cli::{Commands, SemanticSub};
use anyhow::{bail, Result};

pub(crate) async fn dispatch(command: Commands) -> Result<()> {
    match command {
        Commands::Task { command, project } => {
            let project = project.unwrap_or_default();
            crate::cmd::hub_task::run(&project, command).await
        }
        Commands::Search {
            query,
            limit,
            threshold,
            ..
        } => crate::cmd::hub_docs::search(&query, limit, threshold).await,
        Commands::Read { slug } => {
            let md = crate::cmd::hub_docs::read(&slug).await?;
            print!("{md}");
            Ok(())
        }
        Commands::Docs { command } => match command {
            crate::cmd::cli::DocsSub::Ls { prefix } => crate::cmd::hub_docs::docs_ls(&prefix).await,
            crate::cmd::cli::DocsSub::Create { slug, stdin, md } => {
                crate::cmd::hub_docs::docs_create(&slug, stdin, &md).await
            }
            crate::cmd::cli::DocsSub::Append {
                slug,
                stdin,
                text,
                force,
            } => crate::cmd::hub_docs::docs_append(&slug, stdin, &text, force).await,
            crate::cmd::cli::DocsSub::Replace {
                slug,
                stdin,
                md,
                base,
            } => crate::cmd::hub_docs::docs_replace(&slug, stdin, &md, &base).await,
            crate::cmd::cli::DocsSub::Delete { slug } => {
                crate::cmd::hub_docs::docs_delete(&slug).await
            }
            crate::cmd::cli::DocsSub::Tree => crate::cmd::hub_docs::docs_tree().await,
        },
        Commands::Semantic { command } => semantic_run(command).await,
        Commands::Projects => crate::cmd::hub_lists::projects_hub().await,
        Commands::Cron { command } => crate::cmd::hub_lists::cron_hub(command).await,
        Commands::Person { command } => person_hub(command).await,
        other => not_on_hub(other),
    }
}

fn not_on_hub(command: Commands) -> Result<()> {
    let name = match command {
        Commands::Goal { .. } => "goal",
        Commands::Blob { .. } => "blob",
        Commands::Shot { .. } => "shot",
        Commands::Doctor { .. } => "doctor",
        _ => "команда",
    };
    bail!("hub-режим: команда «{name}» не поддерживается. Аналоги: задачи/issues — uwuwu-cli task, вики — docs/read, ассеты — assets/ репо hub")
}

/// Персона = приватный md-док вики (Experience:/personal/PERSON): hub —
/// docs/Experience/person/PERSON.md через git-клон (show) и ff-push
/// (append/replace — контракт записи вики).
/// Hub-репо по умолчанию: установка закреплена за yurvon-screamo/uwuwu-hub.
/// Пустой env не роняет CLI зависаниями на пустом repo — отдаём дефолт.
pub fn hub_repo_default() -> String {
    // пустая строка в env == не задана: дефолт установки
    std::env::var("UWUWU_HUB_REPO")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "yurvon-screamo/uwuwu-hub".into())
}

async fn person_hub(command: crate::cmd::cli::PersonSub) -> Result<()> {
    use crate::store::doc_store::DocStore;
    let path_guard = |value: &str| {
        if !std::path::Path::new(value).is_file() {
            bail!("--md ждёт путь к файлу; для текста из stdin — --stdin")
        }
        Ok(())
    };
    let slug = "Experience/person/PERSON";
    let store = crate::store::github_doc_store::GitHubDocStore::from_env(hub_repo_default());
    match command {
        crate::cmd::cli::PersonSub::Show => {
            let md = store
                .read(&crate::store::doc_store::DocRef { slug: slug.into() })
                .await?;
            println!("{md}");
            Ok(())
        }
        crate::cmd::cli::PersonSub::Append { stdin, text } => {
            let addition = match stdin {
                true => std::io::read_to_string(std::io::stdin())?,
                false => text.join(" "),
            };
            // append_doc сам читает текущий док и конкатенирует — передаём
            // только addition (вариант с ручным склеиванием удваивал тело);
            // дока нет — create со шапкой (семантика person «создать»).
            if store.exists(slug).await? {
                store.append_doc(slug, &addition).await?;
            } else {
                store.create_doc(slug, &addition).await?;
            }
            println!("персона дописана");
            Ok(())
        }
        crate::cmd::cli::PersonSub::Replace { stdin, md } => {
            if let Some(path) = &md {
                path_guard(path)?;
            }
            let body = match (stdin, &md) {
                (true, _) => std::io::read_to_string(std::io::stdin())?,
                (false, Some(path)) => std::fs::read_to_string(path)?,
                _ => String::new(),
            };
            // create-or-replace: дока нет — create со шапкой (frontmatter+
            // H1 из тайтла слага, frontmatter-гард входа)
            if store.exists(slug).await? {
                store.replace_doc(slug, &body, None).await?;
            } else {
                store.create_doc(slug, &body).await?;
            }
            println!("персона перезаписана");
            Ok(())
        }
    }
}

async fn semantic_run(command: SemanticSub) -> Result<()> {
    let cfg = crate::semantic::SemanticConfig::from_env();
    match command {
        SemanticSub::Rebuild { resume } => {
            let report = crate::semantic::rebuild(&cfg, None, resume).await?;
            println!(
                "пересбор: {} встроено, коротких пропущено {}, время {:?}",
                report.embedded, report.skipped_short, report.elapsed
            );
            Ok(())
        }
        SemanticSub::Status => {
            for line in crate::semantic::status(&cfg).await? {
                println!("{line}");
            }
            Ok(())
        }
        SemanticSub::Sync => {
            for line in crate::semantic::sync_once(&cfg).await? {
                println!("{line}");
            }
            Ok(())
        }
    }
}
