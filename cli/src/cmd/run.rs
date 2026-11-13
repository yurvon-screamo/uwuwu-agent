//! CLI-маршрутизация hub-режима (задача
//! 20261112155302-ttyp35g): task/search/read/docs/semantic/projects/person/
//! cron — через uwuwu-hub (TaskStore/DocStore + gh).

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
        Commands::Docs { command } => {
            let crate::cmd::cli::DocsSub::Ls { prefix } = command;
            crate::cmd::hub_docs::docs_ls(&prefix).await
        }
        Commands::Semantic { command } => semantic_run(command).await,
        Commands::Projects => projects_hub().await,
        Commands::Cron { command } => cron_hub(command).await,
        Commands::Person { command } => person_hub(command).await,
        other => not_on_hub(other),
    }
}

fn not_on_hub(command: Commands) -> Result<()> {
    let name = match command {
        Commands::Goal { .. } => "goal",
        Commands::Blob { .. } => "blob",
        Commands::Shot { .. } => "shot",
        Commands::Kernel { .. } => "kernel",
        Commands::Doctor { .. } => "doctor",
        _ => "команда",
    };
    bail!("hub-режим: команда «{name}» не поддерживается. Аналоги: задачи/issues — uwuwu-cli task, вики — docs/read, ассеты — assets/ репо hub")
}

/// Проекты = labels `proj:*` репо hub (источник истины — инвентарь срез A).
async fn projects_hub() -> Result<()> {
    let out = gh_labels();
    for project in out {
        println!("{project}");
    }
    Ok(())
}

fn gh_labels() -> Vec<String> {
    let repo = hub_repo_default();
    let r = std::process::Command::new("gh")
        .args([
            "api",
            &format!("repos/{repo}/labels"),
            "-F",
            "per_page=100",
            "--jq",
            r#"[.[].name | select(startswith("proj:")) | sub("proj:"; "")]"#,
        ])
        .output()
        .expect("gh не запустился");
    String::from_utf8_lossy(&r.stdout)
        .lines()
        .map(String::from)
        .collect()
}

/// Кроны = инвентарь крон-задач: issues с frontmatter schedule в статусе
/// Вейт (мастера). Семантика унифицированных кронов (ADR-075) сохраняется.
async fn cron_hub(command: crate::cmd::cli::CronSub) -> Result<()> {
    let crate::cmd::cli::CronSub::List { project } = command;
    let repo = hub_repo_default();
    let labels_filter = match &project {
        Some(p) => format!("proj:{p}"),
        None => String::new(),
    };
    let mut args = vec![
        "api".to_string(),
        format!("repos/{repo}/issues"),
        "-F".to_string(),
        "state=open".to_string(),
        "-F".to_string(),
        "per_page=100".to_string(),
    ];
    if !labels_filter.is_empty() {
        args.push("-f".to_string());
        args.push(format!("labels={labels_filter}"));
    }
    let mut argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    argv.insert(1, "-X");
    argv.insert(2, "GET");
    let data = crate::store::github_gh::gh_json(&argv, None).await?;
    let mut count = 0;
    if let serde_json::Value::Array(items) = &data {
        for issue in items {
            let body = issue["body"].as_str().unwrap_or("");
            let fm = crate::store::github_task::parse_frontmatter_public(body);
            let Some(schedule) = fm.get("schedule") else {
                continue;
            };
            count += 1;
            println!(
                "#{} [Вейт] {} — {}",
                issue["number"],
                issue["title"].as_str().unwrap_or(""),
                schedule
            );
        }
    }
    println!("крон-мастеров: {count}");
    Ok(())
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
            let current = store
                .read(&crate::store::doc_store::DocRef { slug: slug.into() })
                .await
                .unwrap_or_default();
            let mut next = current;
            if !next.ends_with('\n') {
                next.push('\n');
            }
            next.push_str(&addition);
            next.push('\n');
            store.append_doc(slug, &next).await?;
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
            store.replace_doc(slug, &body).await?;
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
