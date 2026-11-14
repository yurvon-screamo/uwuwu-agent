//! Gh-листинги hub-режима: `cron list` (инвентарь крон-мастеров) и
//! `projects` (реестр проектов). Полная пагинация через gh_pages —
//! канон ADR-020: обе команды молча теряли хвост за первой страницей
//! (баг 1086; инстанс 13.11 — cron list показал 2 мастера из 7).

use anyhow::Result;

use crate::store::github_gh::{gh_json, gh_pages, GH_PAGE_SIZE};
use crate::store::github_task::urlencoded;

/// `projects` — labels `proj:*` репо hub (источник истины — инвентарь
/// среза A). Полный обход страниц labels: проекты за краем первой сотни
/// не теряются. Явный query-string: gh-форма `-f/-F` без `-X` — это POST
/// (422 как данные, баг 13.11), форма на GET искажала выборку (e2e 13.11).
pub(crate) async fn projects_hub() -> Result<()> {
    for project in gh_labels().await? {
        println!("{project}");
    }
    Ok(())
}

async fn gh_labels() -> Result<Vec<String>> {
    let repo = crate::cmd::run::hub_repo_default();
    let pages = gh_pages(async move |page| {
        let data = gh_json(
            &[
                "api",
                "-X",
                "GET",
                &format!("repos/{repo}/labels?per_page={GH_PAGE_SIZE}&page={page}"),
            ],
            None,
        )
        .await?;
        Ok(data.as_array().cloned().unwrap_or_default())
    })
    .await?;
    let mut out = Vec::new();
    for label in pages {
        if let Some(name) = label["name"].as_str() {
            if let Some(project) = name.strip_prefix("proj:") {
                out.push(project.to_string());
            }
        }
    }
    Ok(out)
}

/// `cron list` — инвентарь крон-мастеров: issues с клеткой «Расписание»
/// (глобальные issue fields, ADR-098; fm-ключ schedule мёртв — задача 1087;
/// мастера живут в статусе Вейт — канон унифицированных кронов ADR-075).
/// Полный обход страниц issues (gh_pages), явный query-string с urlencoded
/// labels (gh `-F` форма на GET искажала выборку — вероятный соучастник
/// инцидента «2 из 7» 13.11) и пропуск PR-строк: REST /issues возвращает
/// и PR, канон task list/issuesForProject. Формат вывода не меняется.
pub(crate) async fn cron_hub(command: crate::cmd::cli::CronSub) -> Result<()> {
    let crate::cmd::cli::CronSub::List { project } = command;
    let repo = crate::cmd::run::hub_repo_default();
    let filter = project.map(|p| format!("labels={}", urlencoded(&format!("proj:{p}"))));
    let issues = gh_pages(async move |page| {
        let mut query =
            format!("repos/{repo}/issues?state=open&per_page={GH_PAGE_SIZE}&page={page}");
        if let Some(f) = &filter {
            query.push('&');
            query.push_str(f);
        }
        let data = gh_json(&["api", "-X", "GET", &query], None).await?;
        Ok(data.as_array().cloned().unwrap_or_default())
    })
    .await?;
    let mut count = 0;
    for issue in issues {
        if issue.get("pull_request").is_some() {
            continue;
        }
        // Расписание — глобальное поле «Расписание» (ADR-098), payload
        // issue_field_values; fm-ключ schedule мёртв (задача 1087)
        let fields = crate::store::github_issue_fields::fields_of(&issue);
        let Some(schedule) = fields.get(crate::store::github_issue_fields::names::SCHEDULE) else {
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
    println!("крон-мастеров: {count}");
    Ok(())
}
