//! `uwuwu-cli task`: kanban boards «Задачи <Proj>» — create/list/get/edit/
//! comment/close/body, with cron-board fallback for row resolution.

use anyhow::Context as _;
use serde_json::json;

use super::{resolve_row_any_board, truncate, TaskSub};
use crate::affine_client::{AffineClient, AffineConfig};
use crate::affine_comments::{self, first_line, short_id};
use crate::affine_tasks;
use crate::ui_url;

/// Текст из stdin (--stdin) или аргументов (общий паттерн task-команд).
fn read_text(use_stdin: bool, args: &[String]) -> anyhow::Result<String> {
    if use_stdin {
        use std::io::Read as _;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        Ok(buf)
    } else if !args.is_empty() {
        Ok(args.join(" "))
    } else {
        Err(anyhow::anyhow!(
            "нет текста: укажите --stdin или передайте текст аргументом"
        ))
    }
}

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
        } => {
            create(
                &mut client,
                cfg,
                &board,
                CreateArgs {
                    title,
                    ty,
                    stdin,
                    attach,
                },
            )
            .await
        }
        TaskSub::List {
            status,
            project: proj_override,
        } => list(cfg, root, project, status, proj_override).await,
        TaskSub::Get { id, no_body } => {
            get(&mut client, cfg, root, &project, GetArgs { id, no_body }).await
        }
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
                EditArgs {
                    id,
                    patch: affine_tasks::RowPatch {
                        status,
                        title,
                        pr,
                        branch,
                    },
                },
            )
            .await
        }
        TaskSub::Thread { id } => thread(&mut client, cfg, root, &project, id).await,
        TaskSub::Comment {
            id,
            reply,
            stdin,
            text,
        } => {
            comment(
                &mut client,
                cfg,
                root,
                &project,
                CommentArgs {
                    id,
                    reply,
                    stdin,
                    text,
                },
            )
            .await
        }
        TaskSub::Close { id, note } => {
            close_task(&mut client, cfg, root, &project, CloseArgs { id, note }).await
        }
        TaskSub::Body {
            id,
            md,
            replace,
            stdin,
        } => {
            body(
                &mut client,
                cfg,
                root,
                &project,
                BodyArgs {
                    id,
                    md,
                    replace,
                    stdin,
                },
            )
            .await
        }
    }
}

/// Аргументы субкоманд task: один struct на команду (зеркало clap-варианта)
/// вместо россыпи параметров хендлера.
struct CreateArgs {
    title: String,
    ty: Option<String>,
    stdin: bool,
    attach: Vec<std::path::PathBuf>,
}

struct GetArgs {
    id: String,
    no_body: bool,
}

struct EditArgs {
    id: String,
    patch: affine_tasks::RowPatch,
}

struct CommentArgs {
    id: String,
    reply: Option<String>,
    stdin: bool,
    text: Vec<String>,
}

struct CloseArgs {
    id: String,
    note: Option<String>,
}

struct BodyArgs {
    id: String,
    md: Option<String>,
    replace: bool,
    stdin: bool,
}

async fn create(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    board: &affine_tasks::Board,
    args: CreateArgs,
) -> anyhow::Result<()> {
    let task_type = affine_tasks::TaskType::parse(args.ty.as_deref().unwrap_or("dev"))
        .with_context(|| {
            format!(
                "--type '{}': ожидался dev|spike|bug",
                args.ty.as_deref().unwrap_or("")
            )
        })?;
    let body = if args.stdin {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
        buf
    } else {
        String::new()
    };
    let row = affine_tasks::create_task(client, cfg, board, &args.title, &body, task_type).await?;
    println!("создана: {}", row.title);
    println!("  строка: {}", row.row_id);
    println!("  тело:   {}", row.body_doc);
    // аттачи — после «создана»: сбой не прячет id задачи,
    // ретрай create осознан (задача уже существует)
    if !args.attach.is_empty() {
        crate::affine_blob::attach_files(cfg, &row.body_doc, &args.attach).await?;
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
    let mut filtered: Vec<affine_tasks::TaskRow> = rows
        .into_iter()
        .filter(|r| {
            status
                .as_deref()
                .map(|s| r.status.to_lowercase().starts_with(&s.to_lowercase()))
                .unwrap_or(true)
        })
        .collect();
    // тайтлы только показанных строк, одним батч-запросом
    super::fill_display_titles(cfg, &mut filtered).await;
    if filtered.is_empty() {
        println!("Задач нет.");
    }
    println!("{:<13} {:<12} {:<34} PR", "СТАТУС", "СТРОКА", "ЗАГОЛОВОК");
    for r in &filtered {
        println!(
            "{:<13} {:<12} {:<34} {}",
            r.status,
            &r.row_id[..r.row_id.len().min(12)],
            truncate(&super::display_title(r), 32),
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
    args: GetArgs,
) -> anyhow::Result<()> {
    let (_board, mut row) = resolve_row_any_board(client, cfg, root, project, &args.id).await?;
    // шапка: тайтл нужной строки O(1)-запросом, сбой → плейсхолдер
    let title = super::row_display_title(cfg, &mut row).await;
    println!("Задача:   {title}");
    println!("Статус:   {}", row.status);
    if !row.pr.is_empty() {
        println!("PR:       {}", row.pr);
    }
    if !row.branch.is_empty() {
        println!("Ветка:    {}", row.branch);
    }
    println!("Строка:   {}", row.row_id);
    if !row.body_doc.is_empty() {
        print_body_and_comments(client, cfg, &row, !args.no_body).await?;
    }
    Ok(())
}

async fn edit(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    root: &std::path::Path,
    project: &str,
    args: EditArgs,
) -> anyhow::Result<()> {
    let (board, mut row) = resolve_row_any_board(client, cfg, root, project, &args.id).await?;
    affine_tasks::edit_row(client, cfg, &board, &row, &args.patch).await?;
    println!(
        "обновлено: {}",
        super::row_display_title(cfg, &mut row).await
    );
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
    let bot = bot_user_id();
    let threads = affine_tasks::thread_comments(client, cfg, &row).await?;
    if threads.is_empty() {
        println!("комментов нет");
        return Ok(());
    }
    print!("{}", render_threads(&threads, &bot));
    println!("тем: {}", threads.len());
    Ok(())
}

/// Бот-аккаунт воркспейса (env), fallback — текущий канонический бот.
fn bot_user_id() -> String {
    std::env::var("UWUWU_BOT_USER_ID")
        .unwrap_or_else(|_| "df415887-98cc-4147-b5df-b6c71d8afeae".into())
}

/// Автор лейблом: юзер или агент (бот-аккаунт или агентский маркер в тексте).
fn is_bot_author(user_id: &str, text: &str, bot_user_id: &str) -> bool {
    user_id == bot_user_id
        || text.trim_start().starts_with("🤖")
        || text.trim_start().starts_with("[factory]")
}

/// Лейбл автора: АГЕНТ/ЮЗЕР (для replies — в нижнем регистре).
fn author_label(user_id: &str, text: &str, bot_user_id: &str) -> &'static str {
    if is_bot_author(user_id, text, bot_user_id) {
        "АГЕНТ"
    } else {
        "ЮЗЕР"
    }
}

fn render_threads(threads: &[affine_comments::CommentThread], bot_user_id: &str) -> String {
    let mut out = String::new();
    for thread in threads {
        let who = author_label(&thread.user_id, &thread.text, bot_user_id);
        let mark = if thread.resolved { " [✓]" } else { "" };
        out.push_str(&format!(
            "\n── #{} · {} · {who}{mark}:\n",
            short_id(&thread.id),
            chrono_like_hhmm(thread.created),
        ));
        push_indented(&mut out, &thread.text, "  ");
        for reply in &thread.replies {
            let who = author_label(&reply.user_id, &reply.text, bot_user_id).to_lowercase();
            out.push_str(&format!(
                "     ↳ {} {who}:\n",
                chrono_like_hhmm(reply.created)
            ));
            push_indented(&mut out, &reply.text, "       ");
        }
    }
    out.push('\n');
    out
}

fn push_indented(out: &mut String, text: &str, indent: &str) {
    for line in text.lines() {
        out.push_str(indent);
        out.push_str(line);
        out.push('\n');
    }
}

async fn comment(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    root: &std::path::Path,
    project: &str,
    args: CommentArgs,
) -> anyhow::Result<()> {
    let (_board, row) = resolve_row_any_board(client, cfg, root, project, &args.id).await?;
    let body = read_text(args.stdin, &args.text)?;
    if body.trim().is_empty() {
        anyhow::bail!("пустой коммент");
    }
    match &args.reply {
        Some(prefix) => reply_in_thread(client, cfg, &row, prefix, &body).await,
        None => {
            affine_tasks::comment_task(client, cfg, &row, &body).await?;
            println!("коммент отправлен: {}", truncate(&body, 60));
            Ok(())
        }
    }
}

/// Ответ в существующий тред: GraphQL createReply напрямую в афину
/// (гейтвей reply не экспонирует). Якорь не нужен — нативная ветка.
/// docTitle — функциональный заголовок строки: строгий дозаголовок,
/// без него честная ошибка (пустой docTitle в снапшот недопустим).
async fn reply_in_thread(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    row: &affine_tasks::TaskRow,
    prefix: &str,
    body: &str,
) -> anyhow::Result<()> {
    let ui = super::connect_ui()
        .await
        .map_err(|e| anyhow::anyhow!("{e:#} для ответа в тред"))?;
    let mut row = row.clone();
    let doc_title = super::require_row_title(&ui, cfg, &mut row).await?;
    let threads = affine_tasks::thread_comments(client, cfg, &row).await?;
    let thread = affine_comments::resolve_thread_prefix(&threads, prefix)?;
    ui.create_reply(&thread.id, body, &doc_title).await?;
    println!(
        "ответ в тред #{} отправлен: {}",
        short_id(&thread.id),
        truncate(body, 60)
    );
    Ok(())
}

async fn close_task(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    root: &std::path::Path,
    project: &str,
    args: CloseArgs,
) -> anyhow::Result<()> {
    let (board, mut row) = resolve_row_any_board(client, cfg, root, project, &args.id).await?;
    let patch = affine_tasks::RowPatch {
        status: Some("Done".into()),
        ..Default::default()
    };
    affine_tasks::edit_row(client, cfg, &board, &row, &patch).await?;
    if let Some(note) = args.note {
        affine_tasks::comment_task(client, cfg, &row, &note).await?;
    }
    println!("закрыта: {}", super::row_display_title(cfg, &mut row).await);
    Ok(())
}

async fn body(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    root: &std::path::Path,
    project: &str,
    args: BodyArgs,
) -> anyhow::Result<()> {
    let (_board, mut row) = resolve_row_any_board(client, cfg, root, project, &args.id).await?;
    let title = super::row_display_title(cfg, &mut row).await;
    if row.body_doc.is_empty() {
        anyhow::bail!("у задачи «{title}» нет тела-дока");
    }
    let text = match args.md {
        // clap запрещает --stdin вместе с --md; guard фиксирует запрет и здесь
        Some(path) if !args.stdin => {
            std::fs::read_to_string(&path).with_context(|| format!("--md {path}"))?
        }
        _ => {
            let mut buf = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
            buf
        }
    };
    if args.replace {
        let n_diagrams = affine_tasks::replace_body(client, cfg, &row.body_doc, &text).await?;
        println!("тело заменено: {title} (диаграмм: {n_diagrams})");
    } else {
        let (n_diagrams, n_deleted) =
            affine_tasks::append_body_markdown(client, cfg, &row.body_doc, &text).await?;
        println!("тело дописано: {title} (диаграмм: {n_diagrams}, заменено/удалено: {n_deleted})");
    }
    Ok(())
}

/// Детали задачи: ссылка на тело всегда; экспорт markdown и картинки —
/// только без --no-body; комменты — всегда (быстрая проверка статуса).
async fn print_body_and_comments(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    row: &affine_tasks::TaskRow,
    include_body: bool,
) -> anyhow::Result<()> {
    let ui = ui_url();
    println!(
        "Тело:     {ui}/workspace/{}/{}",
        cfg.workspace, row.body_doc
    );
    if include_body {
        print_body_markdown(client, cfg, row).await?;
    }
    print_comments_brief(client, cfg, row).await?;
    Ok(())
}

/// Экспорт тела markdown + выгрузка картинок в /tmp/uwuwu-task-<row>/.
async fn print_body_markdown(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    row: &affine_tasks::TaskRow,
) -> anyhow::Result<()> {
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
    Ok(())
}

/// Свежие комменты карточки — краткий рендер.
async fn print_comments_brief(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    row: &affine_tasks::TaskRow,
) -> anyhow::Result<()> {
    let threads = affine_tasks::recent_comments(client, cfg, row, 5).await?;
    if !threads.is_empty() {
        println!("Комменты:");
        print!("{}", render_threads_brief(&threads, &bot_user_id()));
    }
    Ok(())
}

/// Краткий рендер для карточки: по строке на коммент/reply, усечка 90.
fn render_threads_brief(threads: &[affine_comments::CommentThread], bot_user_id: &str) -> String {
    let mut out = String::new();
    for thread in threads {
        let who = author_label(&thread.user_id, &thread.text, bot_user_id);
        let mark = if thread.resolved { " [✓]" } else { "" };
        out.push_str(&format!(
            "  • #{} {who}{mark}: {}\n",
            short_id(&thread.id),
            truncate(first_line(&thread.text), 90),
        ));
        for reply in &thread.replies {
            let who = author_label(&reply.user_id, &reply.text, bot_user_id).to_lowercase();
            out.push_str(&format!(
                "      ↳ {who}: {}\n",
                truncate(first_line(&reply.text), 90),
            ));
        }
    }
    out
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

    fn thread(
        id: &str,
        user: &str,
        text: &str,
        replies: Vec<affine_comments::CommentReply>,
    ) -> affine_comments::CommentThread {
        affine_comments::CommentThread {
            id: id.into(),
            text: text.into(),
            user_id: user.into(),
            created: 86_400_000,
            resolved: false,
            replies,
        }
    }

    fn reply(user: &str, text: &str) -> affine_comments::CommentReply {
        affine_comments::CommentReply {
            id: "r".into(),
            text: text.into(),
            user_id: user.into(),
            created: 86_400_000,
        }
    }

    #[test]
    fn render_threads_shows_nested_replies_with_ids() {
        let threads = vec![
            thread(
                "aaaa1111-2222",
                "user-x",
                "вопрос юзера",
                vec![reply("bot-y", "ответ агента")],
            ),
            thread("bbbb3333-4444", "bot-y", "🤖 статус", vec![]),
        ];

        let rendered = render_threads(&threads, "bot-y");

        assert!(rendered.contains("#aaaa1111"), "{rendered}");
        assert!(rendered.contains("ЮЗЕР:"), "{rendered}");
        assert!(rendered.contains("↳"), "reply missing: {rendered}");
        assert!(rendered.contains("АГЕНТ:"), "bot thread head: {rendered}");
    }

    #[test]
    fn render_threads_brief_is_one_line_per_comment() {
        let threads = vec![thread(
            "aaaa1111-2222",
            "user-x",
            "первая строка\nвторая строка",
            vec![reply("bot-y", "ответ")],
        )];

        let rendered = render_threads_brief(&threads, "bot-y");

        let non_empty: Vec<&str> = rendered.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(non_empty.len(), 2, "{rendered}");
        assert!(non_empty[0].contains("первая строка"));
        assert!(!non_empty[0].contains("вторая"));
    }

    #[test]
    fn bot_author_detected_by_user_id_or_marker() {
        assert!(is_bot_author("bot-y", "текст", "bot-y"));
        assert!(is_bot_author("human", "🤖 привет", "bot-y"));
        assert!(!is_bot_author("human", "обычный текст", "bot-y"));
    }
}
