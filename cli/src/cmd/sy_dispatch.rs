//! Handlers всех команд uwuwu-cli на SiYuan-бекенде (единственный).

use crate::cmd::cli::{Commands, CronSub, TaskSub};
use crate::siyuan::SiYuan;
use crate::siyuan_task as syt;
use anyhow::{anyhow, Result};
use serde_json::json;

pub(crate) async fn dispatch(command: Commands) -> anyhow::Result<()> {
    match command {
        Commands::Task { command, project } => {
            let project = project.unwrap_or_default();
            task_run(&project, command).await
        }
        Commands::Cron { command } => cron_run(command).await,
        Commands::Search { query, limit, .. } => search_run(&query, limit).await,
        Commands::Read { slug } => read_run(&slug).await,
        Commands::Blob { key, out } => blob_run(&key, out).await,
        Commands::Projects => projects_run().await,
        Commands::Shot { .. } => Err(anyhow!(
            "shot: не портирован на SiYuan (v1) — headless chromium на http://127.0.0.1:6806 с Access Auth Code"
        )),
    }
}

pub(crate) async fn task_run(project: &str, command: TaskSub) -> Result<()> {
    if project.is_empty() {
        anyhow::bail!("--project <имя> обязателен (uwuwu | origa | …)");
    }
    let sy = SiYuan::from_env()?;
    match command {
        TaskSub::List { status, .. } => {
            let board = syt::find_board(&sy, project).await?;
            let rows = syt::read_rows(&sy, &board).await?;
            let mut filtered: Vec<_> = rows
                .into_iter()
                .filter(|r| {
                    status
                        .as_deref()
                        .map(|s| r.status.to_lowercase().starts_with(&s.to_lowercase()))
                        .unwrap_or(true)
                })
                .collect();
            if filtered.is_empty() {
                println!("Задач нет.");
            }
            println!("{:<13} {:<12} {:<34} PR", "СТАТУС", "СТРОКА", "ЗАГОЛОВОК");
            filtered.sort_by(|a, b| a.status.cmp(&b.status));
            for r in &filtered {
                println!(
                    "{:<13} {:<12} {:<34} {}",
                    r.status,
                    &r.item_id[..r.item_id.len().min(12)],
                    truncate(&r.title, 32),
                    r.pr
                );
            }
            Ok(())
        }
        TaskSub::Get { id, no_body } => {
            let board = syt::find_board(&sy, project).await?;
            let task = syt::resolve(&sy, &board, &id).await?;
            println!("Задача:   {}", task.title);
            println!("Статус:   {}", task.status);
            println!("Строка:   {}", task.item_id);
            if !task.branch.is_empty() {
                println!("Ветка:    {}", task.branch);
            }
            if !task.pr.is_empty() {
                println!("PR:       {}", task.pr);
            }
            if !no_body {
                let md = sy
                    .export_md(&board.notebook, &format!("{}/{}", board.hpath, task.title))
                    .await
                    .unwrap_or_default();
                println!("\n----- тело -----\n{md}");
            }
            Ok(())
        }
        TaskSub::Create { title, r#type: ty, stdin, attach } => {
            if !attach.is_empty() {
                anyhow::bail!("siyuan-бекенд: --attach не поддерживается (вставь картинку в UI)");
            }
            let ctx = if stdin { read_stdin()? } else { String::new() };
            let (item, body) = syt::create(&sy, project, &title, ty.as_deref().unwrap_or("dev"), &ctx).await?;
            println!("создана: {title}\n  строка: {item}\n  тело:   {body}");
            Ok(())
        }
        TaskSub::Edit { id, status, title, pr, branch } => {
            if title.is_some() {
                anyhow::bail!("siyuan-бекенд: переименование задачи не поддерживается (переименуй саб-док в UI)");
            }
            let board = syt::find_board(&sy, project).await?;
            let task = syt::resolve(&sy, &board, &id).await?;
            syt::edit(&sy, &board, &task, status.as_deref(), branch.as_deref(), pr.as_deref()).await?;
            println!("обновлено: {}", task.title);
            Ok(())
        }
        TaskSub::Comment { id, reply, stdin, text } => {
            let msg = collect_text(stdin, text)?;
            let board = syt::find_board(&sy, project).await?;
            let task = syt::resolve(&sy, &board, &id).await?;
            syt::comment(&sy, &board, &task, "AGENTS", &msg, reply.as_deref()).await?;
            println!("коммент отправлен: {}", truncate(&msg, 60));
            Ok(())
        }
        TaskSub::Thread { id } => {
            let board = syt::find_board(&sy, project).await?;
            let task = syt::resolve(&sy, &board, &id).await?;
            let md = sy
                .export_md(&board.notebook, &format!("{}/{}", board.hpath, task.title))
                .await
                .unwrap_or_default();
            print_threads(&md);
            Ok(())
        }
        TaskSub::Close { id, note } => {
            let board = syt::find_board(&sy, project).await?;
            let task = syt::resolve(&sy, &board, &id).await?;
            syt::close(&sy, &board, &task, note.as_deref()).await?;
            println!("закрыта: {}", task.title);
            Ok(())
        }
        TaskSub::Rm { id } => {
            let board = syt::find_board(&sy, project).await?;
            let task = syt::resolve(&sy, &board, &id).await?;
            sy.mcp_tool(
                "database",
                json!({ "action": "item_remove", "id": board.av_id, "itemIDs": [task.item_id] }),
            )
            .await?;
            if let Some(oid) = syt::body_doc_id(&sy, &board, &task.title).await? {
                let _ = sy.remove_doc(&oid).await;
            }
            println!("удалена: {}", task.title);
            Ok(())
        }
        TaskSub::Body { id, replace, replace_section, md, stdin } => {
            let content = read_body_input(md.as_deref(), stdin)?;
            let board = syt::find_board(&sy, project).await?;
            let task = syt::resolve(&sy, &board, &id).await?;
            let body_path = format!("{}/{}", board.hpath, task.title);
            let existing = sy.export_md(&board.notebook, &body_path).await.unwrap_or_default();
            let new_md: String = if replace || replace_section.is_some() {
                if let Some(section) = &replace_section {
                    replace_md_section(&existing, section, &content)
                } else {
                    content.clone()
                }
            } else {
                format!("{existing}\n{content}")
            };
            let old = syt::body_doc_id(&sy, &board, &task.title).await?;
            if let Some(oid) = old {
                sy.remove_doc(&oid).await?;
            }
            let nid = sy.create_doc_md(&board.notebook, &body_path, &new_md).await?;
            println!("тело обновлено: {body_path} ({nid})");
            Ok(())
        }
    }
}

// ——— cron: строки доски «Кроны <proj>» ———

pub(crate) async fn cron_run(command: CronSub) -> Result<()> {
    let sy = SiYuan::from_env()?;
    match command {
        CronSub::List => cron_list(&sy).await,
        CronSub::Set { title, r#in, every, at, cron_expr, project, stdin } => {
            let schedule = cron_schedule(&r#in, &every, &at, &cron_expr)?;
            let prompt = if stdin { read_stdin()? } else { "<промпт крона>".into() };
            let proj = project.unwrap_or_else(|| "uwuwu".into());
            let board = syt::find_board_cron(&sy, &proj).await?;
            // тело крона = саб-док доски
            let body_h = format!("{}/{}", board.hpath, title);
            if syt::body_doc_id(&sy, &board, &title).await?.is_some() {
                anyhow::bail!("крон «{title}» уже есть");
            }
            let body = sy.create_doc_md(&board.notebook, &body_h, &format!("# {title}\n\n```\n{prompt}\n```\n")).await?;
            let item = sy.db_item_add(&board.av_id, &title).await?;
            // клетки Schedule/Status/Last Run (текст)
            set_text_cells(&sy, &board.av_id, &item, &[("Schedule", &schedule), ("Status", "active"), ("Last Run", "")]).await?;
            println!("крон поставлен: {title}\n  расписание: {schedule}\n  строка: {item}\n  тело:   {body}");
            Ok(())
        }
        CronSub::Pause { id } => cron_set_status(&sy, &id, "paused", "остановлен").await,
        CronSub::Resume { id } => cron_set_status(&sy, &id, "active", "запущен").await,
        CronSub::Rm { id } => {
            let board = match syt::find_board_cron(&sy, "uwuwu").await {
                Ok(b) => b,
                Err(_) => syt::find_board(&sy, "uwuwu").await?,
            };
            let task = syt::resolve(&sy, &board, &id).await?;
            sy.mcp_tool(
                "database",
                json!({ "action": "item_remove", "id": board.av_id, "itemIDs": [task.item_id] }),
            )
            .await?;
            if let Some(oid) = syt::body_doc_id(&sy, &board, &task.title).await? {
                let _ = sy.remove_doc(&oid).await;
            }
            println!("крон удалён: {}", task.title);
            Ok(())
        }
    }
}

async fn cron_list(sy: &SiYuan) -> Result<()> {
    let board = syt::find_board_cron(sy, "uwuwu").await?;
    let rows = syt::read_rows(sy, &board).await?;
    println!("{:<10} {:<12} {:<28} {:<14} LAST RUN", "СТАТУС", "СТРОКА", "ЗАГОЛОВОК", "SCHEDULE");
    for r in &rows {
        println!(
            "{:<10} {:<12} {:<28}",
            r.status,
            &r.item_id[..r.item_id.len().min(12)],
            truncate(&r.title, 26)
        );
    }
    Ok(())
}

async fn cron_set_status(sy: &SiYuan, id: &str, status: &str, verb: &str) -> Result<()> {
    let board = syt::find_board_cron(sy, "uwuwu").await?;
    let task = syt::resolve(sy, &board, id).await?;
    set_text_cells(sy, &board.av_id, &task.item_id, &[("Status", status)]).await?;
    println!("крон {verb}: {}", task.title);
    Ok(())
}

async fn set_text_cells(sy: &SiYuan, av: &str, item: &str, cells: &[(&str, &str)]) -> Result<()> {
    let keys = sy.mcp_tool("database", json!({ "action": "keys", "id": av })).await?;
    let arr = keys
        .pointer("/data/keys")
        .or_else(|| keys.get("keys"))
        .and_then(|k| k.as_array())
        .cloned()
        .unwrap_or_default();
    let find = |name: &str| -> Option<String> {
        arr.iter()
            .find(|k| k.get("name").and_then(|n| n.as_str()) == Some(name))
            .and_then(|k| k.get("id").and_then(|i| i.as_str()).map(String::from))
    };
    for (name, value) in cells {
        if let Some(k) = find(name) {
            sy.mcp_tool(
                "database",
                json!({ "action": "item_update", "id": av, "keyID": k, "itemID": item,
                        "value": { "text": { "content": value } } }),
            )
            .await?;
        }
    }
    Ok(())
}

fn cron_schedule(in_: &Option<String>, every: &Option<String>, at: &Option<String>, expr: &Option<String>) -> Result<String> {
    let opts = [in_.as_deref(), every.as_deref(), at.as_deref(), expr.as_deref()];
    let n = opts.iter().filter(|o| o.is_some()).count();
    if n != 1 {
        anyhow::bail!("нужен ровно один из --in / --every / --at / --cron (получено {n})");
    }
    Ok(opts.into_iter().flatten().next().unwrap().to_string())
}

// ——— docs/search/blob/projects ———

pub(crate) async fn search_run(query: &str, limit: usize) -> Result<()> {
    let sy = SiYuan::from_env()?;
    println!("Searching for: \"{query}\"\n");
    let rows = sy.search(query, limit).await?;
    for (i, r) in rows.iter().enumerate() {
        let hpath = r.get("hpath").and_then(|h| h.as_str()).unwrap_or("");
        let content = r.get("content").and_then(|c| c.as_str()).unwrap_or("");
        println!("{}. {}\n    {}", i + 1, hpath, content.replace('\n', " "));
    }
    if rows.is_empty() {
        println!("ничего не найдено");
    }
    Ok(())
}

pub(crate) async fn read_run(slug: &str) -> Result<()> {
    let sy = SiYuan::from_env()?;
    if let Some(rest) = slug.strip_prefix("sofa/") {
        let post = crate::sofa::get(rest).await?;
        println!("# {}\n\n{}", post.title, post.body);
        return Ok(());
    }
    let title = slug.rsplit('/').next().unwrap_or(slug);
    let rows = sy
        .sql(&format!(
            "select hpath, box from blocks where type='d' and content = '{}' limit 5",
            title.replace('\'', "''")
        ))
        .await?;
    if let Some(r) = rows.first() {
        let hpath = r.get("hpath").and_then(|h| h.as_str()).unwrap_or("");
        let box_id = r.get("box").and_then(|b| b.as_str()).unwrap_or("");
        for nb in sy.notebooks().await? {
            if nb.get("id").and_then(|i| i.as_str()) == Some(box_id) {
                let name = nb.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let md = sy.export_md(name, hpath).await?;
                print!("{md}");
                return Ok(());
            }
        }
    }
    Err(anyhow!("док «{slug}» не найден"))
}

pub(crate) async fn blob_run(key: &str, out: Option<String>) -> Result<()> {
    let sy = SiYuan::from_env()?;
    let path = format!("assets/{key}");
    let bytes = sy.download_file(&path).await?;
    let name = out.unwrap_or_else(|| key.to_string());
    if name == "-" {
        use std::io::Write;
        std::io::stdout().write_all(&bytes)?;
    } else {
        std::fs::write(&name, &bytes)?;
        println!("{key} → {} bytes → {}", bytes.len(), name);
    }
    Ok(())
}

pub(crate) async fn projects_run() -> Result<()> {
    let sy = SiYuan::from_env()?;
    for nb in sy.notebooks().await? {
        if nb.get("closed").and_then(|c| c.as_bool()).unwrap_or(false) {
            continue;
        }
        println!("{}", nb.get("name").and_then(|n| n.as_str()).unwrap_or(""));
    }
    Ok(())
}

// ——— утилиты ———

/// Секция «## X» в markdown: заменить содержимое, заголовок сохранить.
fn replace_md_section(md: &str, section: &str, content: &str) -> String {
    let mut out = String::new();
    let mut in_section = false;
    let mut replaced = false;
    let needle = format!("## {section}");
    for line in md.lines() {
        if line.starts_with("## ") {
            in_section = line.trim() == needle;
            if in_section {
                out.push_str(&needle);
                out.push('\n');
                out.push_str(content.trim());
                out.push('\n');
                replaced = true;
                continue;
            }
        }
        if !in_section {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !replaced {
        out.push_str(&format!("\n## {section}\n{content}\n"));
    }
    out
}

fn print_threads(md: &str) {
    let mut printing = false;
    for line in md.lines() {
        if line.starts_with("## ") {
            printing = line.starts_with("## Комменты");
            if printing {
                println!("{line}");
            }
            continue;
        }
        if printing {
            println!("{line}");
        }
    }
    if !printing {
        println!("комментов нет");
    }
}

fn read_body_input(md_file: Option<&str>, stdin: bool) -> Result<String> {
    if let Some(f) = md_file {
        return Ok(std::fs::read_to_string(f)?);
    }
    if stdin {
        return read_stdin();
    }
    anyhow::bail!("нужно --stdin или --md <файл>")
}

fn read_stdin() -> Result<String> {
    use std::io::Read;
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s)
}

fn collect_text(stdin: bool, text: Vec<String>) -> Result<String> {
    if stdin {
        read_stdin()
    } else {
        Ok(text.join(" "))
    }
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
