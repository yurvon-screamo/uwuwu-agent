//! `uwuwu-cli task`: kanban boards «Задачи <Proj>» — create/list/get/edit/
//! comment/close/body, with cron-board fallback for row resolution.

use anyhow::Context as _;
use serde_json::json;

use super::{resolve_row_any_board, truncate, TaskSub};
use crate::affine_client::{AffineClient, AffineConfig};
use crate::affine_tasks;
use crate::{facts, ui_url};

pub(crate) async fn run(
    cfg: &AffineConfig,
    root: &std::path::Path,
    project: Option<String>,
    command: TaskSub,
) -> anyhow::Result<()> {
    // required-флаг в clap не живёт после субкоманды — проверяем сами
    let Some(project) = project else {
        anyhow::bail!("--project <имя> обязателен (uwuwu | origa | foilcap | …)");
    };
    let mut client = AffineClient::connect(cfg).await?;
    let board = affine_tasks::find_board(&mut client, cfg, &project)
        .await
        .with_context(|| format!("доска проекта «{project}» не найдена"))?;
    match command {
        TaskSub::Create {
            title,
            r#type: ty,
            stdin,
            attach,
        } => create(&mut client, cfg, &board, &title, ty, stdin, attach).await,
        TaskSub::List {
            status,
            project: proj_override,
        } => list(cfg, root, project, status, proj_override).await,
        TaskSub::Get { id } => get(&mut client, cfg, root, &project, id).await,
        TaskSub::Edit {
            id,
            status,
            title,
            pr,
            branch,
        } => {
            edit(
                &mut client,
                cfg,
                root,
                &project,
                id,
                affine_tasks::RowPatch {
                    status,
                    title,
                    pr,
                    branch,
                },
            )
            .await
        }
        TaskSub::Thread { id } => thread(&mut client, cfg, root, &project, id).await,
        TaskSub::Comment { id, stdin, text } => {
            comment(&mut client, cfg, root, &project, id, stdin, text).await
        }
        TaskSub::Close { id, note } => close_task(&mut client, cfg, root, &project, id, note).await,
        TaskSub::Body { id, md, replace } => {
            body(&mut client, cfg, root, &project, id, md, replace).await
        }
    }
}

async fn create(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    board: &affine_tasks::Board,
    title: &str,
    ty: Option<String>,
    stdin: bool,
    attach: Vec<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let task_type =
        affine_tasks::TaskType::parse(ty.as_deref().unwrap_or("dev")).with_context(|| {
            format!(
                "--type '{}': ожидался dev|spike|bug",
                ty.as_deref().unwrap_or("")
            )
        })?;
    let body = if stdin {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
        buf
    } else {
        String::new()
    };
    let row = affine_tasks::create_task(client, cfg, board, title, &body, task_type).await?;
    println!("создана: {}", row.title);
    println!("  строка: {}", row.row_id);
    println!("  тело:   {}", row.body_doc);
    // аттачи — после «создана»: сбой не прячет id задачи,
    // ретрай create осознан (задача уже существует)
    if !attach.is_empty() {
        crate::affine_blob::attach_files(cfg, &row.body_doc, &attach).await?;
    }
    Ok(())
}

/// List идёт своим коннектом: --project субкоманды переопределяет доску.
async fn list(
    cfg: &AffineConfig,
    root: &std::path::Path,
    project: String,
    status: Option<String>,
    proj_override: Option<String>,
) -> anyhow::Result<()> {
    let project = proj_override
        .or(Some(project))
        .unwrap_or_else(|| "uwuwu".to_string());
    let mut client = AffineClient::connect(cfg).await?;
    let board = affine_tasks::find_board(&mut client, cfg, &project)
        .await
        .with_context(|| format!("доска проекта «{project}» не найдена"))?;
    let rows = affine_tasks::read_rows(&mut client, cfg, &board, root).await?;
    let filtered: Vec<_> = rows
        .iter()
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
    for r in &filtered {
        println!(
            "{:<13} {:<12} {:<34} {}",
            r.status,
            &r.row_id[..r.row_id.len().min(12)],
            truncate(&r.title, 32),
            r.pr
        );
    }
    Ok(())
}

async fn get(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    root: &std::path::Path,
    project: &str,
    id: String,
) -> anyhow::Result<()> {
    let (_board, row) = resolve_row_any_board(client, cfg, root, project, &id).await?;
    println!("Задача:   {}", row.title);
    println!("Статус:   {}", row.status);
    if !row.pr.is_empty() {
        println!("PR:       {}", row.pr);
    }
    if !row.branch.is_empty() {
        println!("Ветка:    {}", row.branch);
    }
    println!("Строка:   {}", row.row_id);
    if !row.body_doc.is_empty() {
        print_body_and_comments(client, cfg, &row).await?;
    }
    Ok(())
}

async fn edit(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    root: &std::path::Path,
    project: &str,
    id: String,
    patch: affine_tasks::RowPatch,
) -> anyhow::Result<()> {
    let (board, row) = resolve_row_any_board(client, cfg, root, project, &id).await?;
    affine_tasks::edit_row(client, cfg, &board, &row, &patch).await?;
    println!("обновлено: {}", row.title);
    Ok(())
}

async fn thread(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    root: &std::path::Path,
    project: &str,
    id: String,
) -> anyhow::Result<()> {
    let (_board, row) = resolve_row_any_board(client, cfg, root, project, &id).await?;
    let bot_user_id = std::env::var("UWUWU_BOT_USER_ID")
        .unwrap_or_else(|_| "df415887-98cc-4147-b5df-b6c71d8afeae".into());
    let comments = affine_tasks::thread_comments(client, cfg, &row).await?;
    if comments.is_empty() {
        println!("комментов нет");
        return Ok(());
    }
    let mut topic = 0usize;
    for (text, user_id, created, resolved) in &comments {
        let is_bot = *user_id == bot_user_id
            || text.trim_start().starts_with("🤖")
            || text.trim_start().starts_with("[factory]");
        let time = chrono_like_hhmm(*created);
        let mark = if *resolved { " [✓]" } else { "" };
        if !is_bot {
            topic += 1;
            println!();
            println!("── тема {topic} · {time} · ЮЗЕР{mark}:");
            for line in text.lines() {
                println!("  {line}");
            }
        } else {
            println!("     ↳ {time} агент:");
            for line in text.lines() {
                println!("       {line}");
            }
        }
    }
    println!();
    println!("тем: {topic}");
    Ok(())
}

async fn comment(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    root: &std::path::Path,
    project: &str,
    id: String,
    stdin: bool,
    text: Vec<String>,
) -> anyhow::Result<()> {
    let (_board, row) = resolve_row_any_board(client, cfg, root, project, &id).await?;
    let body = facts::read_text(stdin, &text)?;
    if body.trim().is_empty() {
        anyhow::bail!("пустой коммент");
    }
    affine_tasks::comment_task(client, cfg, &row, &body).await?;
    println!("коммент отправлен: {}", truncate(&body, 60));
    Ok(())
}

async fn close_task(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    root: &std::path::Path,
    project: &str,
    id: String,
    note: Option<String>,
) -> anyhow::Result<()> {
    let (board, row) = resolve_row_any_board(client, cfg, root, project, &id).await?;
    let patch = affine_tasks::RowPatch {
        status: Some("Done".into()),
        ..Default::default()
    };
    affine_tasks::edit_row(client, cfg, &board, &row, &patch).await?;
    if let Some(note) = note {
        affine_tasks::comment_task(client, cfg, &row, &note).await?;
    }
    println!("закрыта: {}", row.title);
    Ok(())
}

async fn body(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    root: &std::path::Path,
    project: &str,
    id: String,
    md: Option<String>,
    replace: bool,
) -> anyhow::Result<()> {
    let (_board, row) = resolve_row_any_board(client, cfg, root, project, &id).await?;
    if row.body_doc.is_empty() {
        anyhow::bail!("у задачи «{}» нет тела-дока", row.title);
    }
    let text = match &md {
        Some(path) => std::fs::read_to_string(path).with_context(|| format!("--md {path}"))?,
        None => {
            let mut buf = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
            buf
        }
    };
    if replace {
        let n_diagrams = affine_tasks::replace_body(client, cfg, &row.body_doc, &text).await?;
        println!("тело заменено: {} (диаграмм: {n_diagrams})", row.title);
    } else {
        let (n_diagrams, n_deleted) =
            affine_tasks::append_body_markdown(client, cfg, &row.body_doc, &text).await?;
        println!(
            "тело дописано: {} (диаграмм: {n_diagrams}, заменено/удалено: {n_deleted})",
            row.title
        );
    }
    Ok(())
}

/// Тело целиком + свежие комменты: task get — единственный cli-путь
/// агента к содержимому; картинки из markdown выгружаются рядом.
async fn print_body_and_comments(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    row: &affine_tasks::TaskRow,
) -> anyhow::Result<()> {
    let ui = ui_url();
    println!(
        "Тело:     {ui}/workspace/{}/{}",
        cfg.workspace, row.body_doc
    );
    if let Ok(e) = client
        .call_tool(
            "export_doc_markdown",
            json!({"workspaceId": cfg.workspace, "docId": row.body_doc}),
        )
        .await
    {
        let md = e.get("markdown").and_then(|m| m.as_str()).unwrap_or("");
        if !md.trim().is_empty() {
            println!("\n----- тело -----\n{md}");
            let imgs = save_body_images(md, &row.row_id).await;
            if !imgs.is_empty() {
                println!("Картинки:");
                for p in imgs {
                    println!("  {p}");
                }
            }
        }
    }
    let comments = affine_tasks::recent_comments(client, cfg, row, 5).await?;
    if !comments.is_empty() {
        println!("Комменты:");
        for (text, _) in comments {
            println!("  • {}", truncate(&text, 90));
        }
    }
    Ok(())
}

/// Выгрузить картинки тела (`affine://blob/<key>` в markdown) в
/// /tmp/uwuwu-task-<row>/ — агент получает локальные пути одним get.
/// UI-кредов нет / шлюз лежит → молча остаёмся на ссылках.
async fn save_body_images(md: &str, row_id: &str) -> Vec<String> {
    const MAX: usize = 10;
    let mut out = Vec::new();
    // собрать уникальные ключи в порядке появления
    let mut keys: Vec<String> = Vec::new();
    for cap in regex::Regex::new(r"affine://blob/([A-Za-z0-9._%~:+-]+)")
        .unwrap()
        .captures_iter(md)
    {
        let raw = cap.get(1).unwrap().as_str().to_string();
        // ключ в URL percent-encoded — декодируем %XX
        let key = percent_decode(&raw);
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    if keys.is_empty() {
        return out;
    }
    let Some(ss) = crate::affine_search_server::ServerSearchConfig::from_env() else {
        return out;
    };
    let Ok(ws_cfg) = AffineConfig::from_env() else {
        return out;
    };
    let dir = format!(
        "/tmp/uwuwu-task-{}",
        row_id
            .trim_start_matches('-')
            .chars()
            .take(8)
            .collect::<String>()
    );
    if std::fs::create_dir_all(&dir).is_err() {
        return out;
    }
    for (i, key) in keys.iter().take(MAX).enumerate() {
        match crate::affine_blob::download_blob(&ss, &ws_cfg.workspace, key).await {
            Ok(bytes) if !bytes.is_empty() => {
                let ext = sniff_ext(&bytes);
                let path = format!("{dir}/img-{}.{ext}", i + 1);
                if std::fs::write(&path, &bytes).is_ok() {
                    out.push(path);
                }
            }
            _ => {}
        }
    }
    out
}

/// Простой %XX-декодер (ключи блобов приезжают URL-encoded).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Расширение по magic bytes (png/jpg/gif/webp, иначе bin).
fn sniff_ext(b: &[u8]) -> &'static str {
    if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        "png"
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else if b.starts_with(b"GIF8") {
        "gif"
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        "webp"
    } else {
        "bin"
    }
}

/// HH:MM из ms-таймстампа (локальная зона машины).
fn chrono_like_hhmm(ms: i64) -> String {
    if ms <= 0 {
        return "—".into();
    }
    // без chrono: дни/остаток от эпохи через секунды, часовой сдвиг берём из локали через `date`
    let secs = ms.div_euclid(1000);
    let today_local = std::process::Command::new("date")
        .arg("+%z")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| "+0000".into());
    let sign: i64 = if today_local.starts_with('-') { -1 } else { 1 };
    let hours_off: i64 = today_local
        .trim_start_matches(['+', '-'])
        .get(..2)
        .and_then(|h| h.parse().ok())
        .unwrap_or(0);
    let local = secs + sign * hours_off * 3600;
    let day_secs = local.rem_euclid(86_400);
    format!("{:02}:{:02}", day_secs / 3600, (day_secs % 3600) / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_decodes_url_encoded_blob_keys() {
        assert_eq!(percent_decode("a%20b.png"), "a b.png");
        assert_eq!(percent_decode("plain.png"), "plain.png");
        // неполная последовательность %XX остаётся как есть
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("a%2"), "a%2");
    }

    #[test]
    fn sniff_ext_detects_magic_bytes() {
        assert_eq!(sniff_ext(&[0x89, b'P', b'N', b'G', 1, 2]), "png");
        assert_eq!(sniff_ext(&[0xFF, 0xD8, 0xFF, 0xE0]), "jpg");
        assert_eq!(sniff_ext(b"GIF89a"), "gif");
        // WEBP: RIFF + размер + WEBP + полезная нагрузка (>12 байт)
        assert_eq!(sniff_ext(b"RIFF\x00\x00\x00\x00WEBPVP8"), "webp");
        assert_eq!(sniff_ext(b"whatever"), "bin");
    }

    #[test]
    fn chrono_like_hhmm_renders_local_time_and_dash_for_missing() {
        // 00:00 UTC в зоне +00:00 → ровно полночь; локаль машины даёт сдвиг
        let rendered = chrono_like_hhmm(86_400_000);
        assert!(rendered.contains(':'));
        assert_eq!(chrono_like_hhmm(0), "—");
    }
}
