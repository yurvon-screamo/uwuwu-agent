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
            let dead_bound = syt::dead_bound_doc_ids(&sy, &rows).await?;
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
                let marker = match r.body_doc.as_deref() {
                    Some(b) if dead_bound.contains(b) => " (!)",
                    _ => "",
                };
                println!(
                    "{:<13} {:<12} {:<34} {}{}",
                    r.status,
                    &r.item_id[..r.item_id.len().min(12)],
                    truncate(&r.title, 32),
                    r.pr,
                    marker
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
                let md = match syt::task_body_doc(&sy, &board, &task).await? {
                    Some(doc) => sy.export_md_by_id(&doc).await.unwrap_or_default(),
                    None => String::new(),
                };
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
            let md = match syt::task_body_doc(&sy, &board, &task).await? {
                Some(doc) => sy.export_md_by_id(&doc).await.unwrap_or_default(),
                None => String::new(),
            };
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
            if let Some(doc) = syt::task_body_doc(&sy, &board, &task).await? {
                let _ = sy.remove_doc(&doc).await;
            }
            println!("удалена: {}", task.title);
            Ok(())
        }
        TaskSub::Body { id, replace, replace_section, md, stdin } => {
            let content = read_body_input(md.as_deref(), stdin)?;
            let board = syt::find_board(&sy, project).await?;
            let task = syt::resolve(&sy, &board, &id).await?;
            let body_path = format!("{}/{}", board.hpath, task.title);
            let doc = syt::task_body_doc(&sy, &board, &task).await?;
            let doc = match doc {
                Some(doc_id) => {
                    write_body_in_place(&sy, &doc_id, replace || replace_section.is_some(), replace_section.as_deref(), &content).await?;
                    doc_id
                }
                None => {
                    // тела нет вовсе: создаю по заголовку (не привязываю — itemID строки неприкосновенен)
                    let initial = if replace || replace_section.is_some() {
                        content.clone()
                    } else {
                        content.trim_start().to_string()
                    };
                    sy.create_doc_md(&board.notebook, &body_path, &initial).await?
                }
            };
            println!("тело обновлено: {body_path} ({doc})");
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
            if !syt::body_doc_by_title(&sy, &board, &title).await?.is_empty() {
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
            if let Some(doc) = syt::task_body_doc(&sy, &board, &task).await? {
                let _ = sy.remove_doc(&doc).await;
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

/// Запись тела in-place: docId сохраняется, привязка строки не рвётся.
/// append — один appendBlock; replace/replace-section — экспорт по id,
/// strip frontmatter (иначе он ляжет литеральным блоком), пересборка контента.
async fn write_body_in_place(
    sy: &SiYuan,
    doc_id: &str,
    rewrite: bool,
    section: Option<&str>,
    content: &str,
) -> Result<()> {
    if !rewrite {
        return sy.append_block_md(doc_id, content).await;
    }
    let existing_raw = sy.export_md_by_id(doc_id).await.unwrap_or_default();
    let existing = strip_frontmatter(&existing_raw);
    let new_md = match section {
        Some(section) => replace_md_section(&existing, section, content),
        None => content.to_string(),
    };
    sy.overwrite_doc_md(doc_id, &new_md).await
}

/// YAML frontmatter из начала markdown (--- ... ---) — live-атрибуты дока.
fn strip_frontmatter(md: &str) -> String {
    let rest = match md.strip_prefix("---\n") {
        Some(rest) => rest,
        None => return md.to_string(),
    };
    match rest.split_once("\n---") {
        Some((_, after)) => after.trim_start_matches('\n').to_string(),
        None => md.to_string(),
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

#[cfg(test)]
mod tests {
    use super::replace_md_section;
    use super::strip_frontmatter;
    use super::task_run;
    use crate::cmd::cli::TaskSub;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const BOARD_DOC: &str = "board-doc";
    const AV_ID: &str = "av-1";
    const CARRIER: &str = "block-1";
    const TITLE_KEY: &str = "key-1";
    const ROW_ID: &str = "item-1";
    const BOUND_DOC: &str = "doc-bound";
    const ORPHAN_DOC: &str = "doc-orphan";

    fn render_rows(with_binding: bool) -> serde_json::Value {
        let mut block = serde_json::json!({ "content": "таск", "refSubtype": "d" });
        if with_binding {
            block["id"] = serde_json::json!(BOUND_DOC);
        }
        serde_json::json!({
            "columns": [{ "id": TITLE_KEY, "name": "Primary Key" }],
            "rows": [{
                "id": ROW_ID,
                "cells": [{ "value": { "keyID": TITLE_KEY, "type": "block", "block": block } }]
            }]
        })
    }

    /// Ответ MCP tools/call(database render) как текст content[0].
    fn mcp_render_response(with_binding: bool) -> String {
        let payload = serde_json::json!({ "action": "render", "data": { "view": render_rows(with_binding) } });
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2,
            "result": { "content": [{ "type": "text", "text": payload.to_string() }] }
        })
        .to_string()
    }

    /// Минимальный мок kernel API SiYuan для body-пути: SQL-роутер по stmt,
    /// MCP initialize/render. Счётчики: createDocWithMd (инвариант == 0)
    /// и тела appendBlock (факт записи в привязанный док).
    async fn mock_siyuan(
        title_doc_ids: Vec<String>,
        with_binding: bool,
    ) -> (MockServer, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
        let server = MockServer::start().await;
        let create_calls = Arc::new(AtomicUsize::new(0));
        let append_bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let sql_create = create_calls.clone();
        let sql_append = append_bodies.clone();
        let title_docs = title_doc_ids.clone();
        Mock::given(method("POST"))
            .and(path("/api/query/sql"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let stmt = body.get("stmt").and_then(|s| s.as_str()).unwrap_or("");
                let data = if stmt.contains("type='av'") {
                    serde_json::json!([{ "id": CARRIER, "markdown": format!("<div data-type=\"NodeAttributeView\" data-av-id=\"{AV_ID}\"></div>") }])
                } else if stmt.contains(&format!("id='{BOUND_DOC}'")) {
                    serde_json::json!([{ "id": BOUND_DOC }])
                } else if stmt.contains("hpath like") {
                    serde_json::json!([{ "id": BOUND_DOC }, { "id": ORPHAN_DOC }])
                } else {
                    serde_json::json!([])
                };
                let _ = &sql_create;
                let _ = &sql_append;
                ResponseTemplate::new(200).set_body_string(serde_json::json!({ "code": 0, "data": data }).to_string())
            })
            .mount(&server)
            .await;

        let create_for_mount = create_calls.clone();
        Mock::given(method("POST"))
            .and(path("/api/filetree/createDocWithMd"))
            .respond_with(move |_req: &wiremock::Request| {
                create_for_mount.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_string(r#"{"code":-1,"msg":"createDocWithMd must not be called"}"#)
            })
            .mount(&server)
            .await;

        let appends = append_bodies.clone();
        Mock::given(method("POST"))
            .and(path("/api/block/appendBlock"))
            .respond_with(move |req: &wiremock::Request| {
                appends
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&req.body).to_string());
                ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":null}"#)
            })
            .mount(&server)
            .await;

        let ids = title_docs.clone();
        Mock::given(method("POST"))
            .and(path("/api/filetree/getIDsByHPath"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let hpath = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
                let data = if hpath.ends_with("/таск") {
                    serde_json::json!(ids)
                } else {
                    serde_json::json!([BOARD_DOC])
                };
                ResponseTemplate::new(200).set_body_string(serde_json::json!({ "code": 0, "data": data }).to_string())
            })
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/notebook/lsNotebooks"))
            .respond_with(|_req: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_string(
                    serde_json::json!({ "code": 0, "data": { "notebooks": [{ "id": "nb-1", "name": "Projects" }] } })
                        .to_string(),
                )
            })
            .mount(&server)
            .await;

        let with_binding = with_binding;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(move |req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("\"initialize\"") {
                    ResponseTemplate::new(200)
                        .set_body_string(
                            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"3"}}}"#,
                        )
                        .append_header("mcp-session-id", "sess-1")
                } else if body.contains("notifications/initialized") {
                    ResponseTemplate::new(200)
                } else if body.contains("tools/call") {
                    ResponseTemplate::new(200).set_body_string(mcp_render_response(with_binding))
                } else {
                    ResponseTemplate::new(200).set_body_string(r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"unexpected"}}"#)
                }
            })
            .mount(&server)
            .await;

        (server, create_calls, append_bodies)
    }

    async fn write_body_via_cli(server: &MockServer) -> anyhow::Result<()> {
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
        let md = tempfile::NamedTempFile::new()?;
        std::fs::write(md.path(), "маркер-контент\n")?;
        task_run(
            "uwuwu",
            TaskSub::Body {
                id: "таск".into(),
                stdin: false,
                md: Some(md.path().to_string_lossy().into_owned()),
                replace: false,
                replace_section: None,
            },
        )
        .await
    }

    /// BDD-сценарий задачи: bound-строка + док-сирота с тем же заголовком →
    /// контент попадает в привязанное тело, docId ответа = привязанный,
    /// новых доков не создаётся (сирота стоит ПЕРВОЙ в ответе getIDsByHPath).
    #[tokio::test]
    #[serial_test::serial]
    async fn body_writes_into_bound_doc_and_never_creates_docs() {
        let (server, create_calls, append_bodies) =
            mock_siyuan(vec![ORPHAN_DOC.to_string(), BOUND_DOC.to_string()], true).await;
        write_body_via_cli(&server).await.unwrap();

        assert_eq!(create_calls.load(Ordering::SeqCst), 0, "createDocWithMd не должен вызываться");
        let appends = append_bodies.lock().unwrap();
        assert_eq!(appends.len(), 1, "один appendBlock в привязанный док");
        assert!(appends[0].contains("маркер-контент"), "контент в appendBlock");
        assert!(appends[0].contains(&format!("\"parentID\":\"{BOUND_DOC}\"")), "append в привязанный док, не в сироту");
    }

    /// Detached-строка (привязки нет) + ≥2 доков с тем же заголовком →
    /// явная ошибка со списком docId, не молчаливая запись в «первый».
    #[tokio::test]
    #[serial_test::serial]
    async fn body_errors_listing_duplicates_for_detached_row() {
        let (server, _create, _append) =
            mock_siyuan(vec![ORPHAN_DOC.to_string(), "doc-b".to_string()], false).await;
        let err = write_body_via_cli(&server).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("несколько доков"), "суть ошибки: {msg}");
        assert!(msg.contains(ORPHAN_DOC) && msg.contains("doc-b"), "список docId в ошибке: {msg}");
    }

    #[test]
    fn strip_frontmatter_cuts_leading_yaml_block() {
        let md = "---\ntitle: т\ndate: x\n---\n\n# Заголовок\n\nтекст\n";
        assert_eq!(strip_frontmatter(md), "# Заголовок\n\nтекст\n");
    }

    #[test]
    fn strip_frontmatter_keeps_markdown_without_frontmatter() {
        let md = "# Обычный док\n\n---\n\nhr внутри текста\n";
        assert_eq!(strip_frontmatter(md), md);
    }

    #[test]
    fn strip_frontmatter_keeps_unterminated_block() {
        let md = "---\ntitle: без закрывашки\n";
        assert_eq!(strip_frontmatter(md), md);
    }

    #[test]
    fn replace_md_section_replaces_body_keeps_heading() {
        let md = "## А\nстарое\n\n## Б\nвторое\n";
        let out = replace_md_section(md, "А", "новое содержимое\n");
        assert!(out.contains("## А\nновое содержимое"));
        assert!(out.contains("## Б\nвторое"));
    }

    #[test]
    fn replace_md_section_appends_missing_section() {
        let md = "## А\nпервое\n";
        let out = replace_md_section(md, "Новая", "контент");
        assert!(out.contains("## Новая\nконтент"));
        assert!(out.contains("## А\nпервое"));
    }
}
