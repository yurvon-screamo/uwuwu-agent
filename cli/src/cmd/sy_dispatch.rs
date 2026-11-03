//! Диспетчер SiYuan-бекенда: те же команды uwuwu-cli, что и на афине,
//! включается env UWUWU_BACKEND=siyuan. Форматы вывода сохранены.

use crate::cmd::cli::{Commands, TaskSub};
use crate::siyuan::SiYuan;
use crate::siyuan_task as syt;
use anyhow::{anyhow, Result};

pub(crate) fn enabled() -> bool {
    std::env::var("UWUWU_BACKEND").unwrap_or_default().eq_ignore_ascii_case("siyuan")
}

/// Обрабатывает команду целиком (ownership), когда включён SiYuan-бекенд.
pub(crate) async fn dispatch(command: Commands) -> anyhow::Result<()> {
    match command {
        Commands::Task { command, project } => {
            let project = project.unwrap_or_default();
            task_run(&project, command).await
        }
        Commands::Search { query, limit, .. } => search_run(&query, limit).await,
        Commands::Read { slug } => read_run(&slug).await,
        Commands::Blob { key, out } => blob_run(&key, out).await,
        Commands::Projects => projects_run().await,
        Commands::Cron { .. } => cron_list("uwuwu").await,
        _ => Err(anyhow!(
            "UWUWU_BACKEND=siyuan: команда пока не портирована на SiYuan-бекенд (афа жива без env)"
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
                if let Some(doc) = syt::body_doc_id(&sy, &board, &task.title).await? {
                    let v = sy
                        .sql(&format!("select content from blocks where id='{doc}'"))
                        .await?;
                    let _ = v;
                    // тело целиком: export
                    let md = sy.export_md(&board.notebook, &format!("{}/{}", board.hpath, task.title)).await?;
                    println!("\n----- тело -----\n{md}");
                } else {
                    println!("(тело не найдено)");
                }
            }
            Ok(())
        }
        TaskSub::Create { title, r#type: ty, stdin, attach } => {
            if stdin || !attach.is_empty() {
                anyhow::bail!("siyuan-бекенд v1: --stdin/--attach при create не поддерживаются (создай тело task body)");
            }
            let (item, body) = syt::create(&sy, project, &title, ty.as_deref().unwrap_or("dev"), "").await?;
            println!("создана: {title}\n  строка: {item}\n  тело:   {body}");
            Ok(())
        }
        TaskSub::Edit { id, status, title, pr, branch } => {
            if title.is_some() {
                anyhow::bail!("siyuan-бекенд v1: переименование задачи не поддерживается");
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
        TaskSub::Close { id, note } => {
            let board = syt::find_board(&sy, project).await?;
            let task = syt::resolve(&sy, &board, &id).await?;
            syt::close(&sy, &board, &task, note.as_deref()).await?;
            println!("закрыта: {}", task.title);
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
            // replace = пересоздать тело (SiYuan не умеет полный rewrite in-place через md)
            let old = syt::body_doc_id(&sy, &board, &task.title).await?;
            if let Some(oid) = old {
                sy.remove_doc(&oid).await?;
            }
            let nid = sy.create_doc_md(&board.notebook, &body_path, &new_md).await?;
            println!("тело обновлено: {body_path} ({nid})");
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
        TaskSub::Rm { id } => {
            let board = syt::find_board(&sy, project).await?;
            let task = syt::resolve(&sy, &board, &id).await?;
            sy.mcp_tool(
                "database",
                serde_json::json!({ "action": "item_remove", "id": board.av_id, "itemIDs": [task.item_id] }),
            )
            .await?;
            if let Some(oid) = syt::body_doc_id(&sy, &board, &task.title).await? {
                let _ = sy.remove_doc(&oid).await;
            }
            println!("удалена: {}", task.title);
            Ok(())
        }
    }
}

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

/// Печать комментов из секции «## Комменты…» тела.
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
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        return Ok(s);
    }
    anyhow::bail!("нужно --stdin или --md <файл>")
}

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
    // slug → hpath: последняя секция = заголовок
    let title = slug.rsplit('/').next().unwrap_or(slug);
    let rows = sy
        .sql(&format!(
            "select hpath, box from blocks where type='d' and content = '{t}' limit 5",
            t = title.replace('\'', "''")
        ))
        .await?;
    if let Some(r) = rows.first() {
        let hpath = r.get("hpath").and_then(|h| h.as_str()).unwrap_or("");
        let box_id = r.get("box").and_then(|b| b.as_str()).unwrap_or("");
        // box → notebook name
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

fn collect_text(stdin: bool, text: Vec<String>) -> Result<String> {
    if stdin {
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        Ok(s)
    } else {
        Ok(text.join(" "))
    }
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// cron-поддержка (list) — строки доски «Кроны <Proj>»
pub(crate) async fn cron_list(project: &str) -> Result<()> {
    let sy = SiYuan::from_env()?;
    let board = syt::find_board_cron(&sy, project).await?;
    let rows = syt::read_rows(&sy, &board).await?;
    println!("{:<10} {:<12} {:<30} LAST RUN", "СТАТУС", "СТРОКА", "ЗАГОЛОВОК");
    for r in &rows {
        println!(
            "{:<10} {:<12} {:<30}",
            r.status,
            &r.item_id[..r.item_id.len().min(12)],
            truncate(&r.title, 28)
        );
    }
    Ok(())
}
