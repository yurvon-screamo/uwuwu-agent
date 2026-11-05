//! Handlers всех команд uwuwu-cli на SiYuan-бекенде (единственный).

use super::read_resolve;
use crate::cmd::cli::{Commands, CronSub, KernelSub, TaskSub};
use crate::markdown;
use crate::md_heading;
use crate::siyuan::{FileFetch, SiYuan};
use crate::siyuan_guard as guard;
use crate::siyuan_guard::{Journal, JournalRow, Probe, Verdict};
use crate::siyuan_task as syt;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

/// Заметка о фоллбеке: строка найдена не на штатной доске задач — прозрачность
/// пути резолва в выводе команды.
fn note_fallback_board(origin: syt::ResolveOrigin, board: &syt::SyBoard) {
    if origin != syt::ResolveOrigin::Tasks {
        println!("доска: {} (фоллбек)", board.hpath);
    }
}

pub(crate) async fn dispatch(command: Commands) -> anyhow::Result<()> {
    match command {
        Commands::Task { command, project } => {
            let project = project.unwrap_or_default();
            task_run(&project, command).await
        }
        Commands::Cron { command } => cron_run(command).await,
        Commands::Search { query, limit, .. } => search_run(&query, limit).await,
        Commands::Read { slug } => {
            let md = read_markdown(&slug).await?;
            print!("{md}");
            Ok(())
        }
        Commands::Blob { key, out } => blob_run(&key, out).await,
        Commands::Projects => projects_run().await,
        Commands::Shot { .. } => Err(anyhow!(
            "shot: не портирован на SiYuan (v1) — headless chromium на http://127.0.0.1:6806 с Access Auth Code"
        )),
        Commands::Kernel { command, timeout } => kernel_run(command, timeout).await,
        Commands::Doctor { project, since } => doctor_run(&project, since).await,
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
            let (board, task, origin) = syt::resolve_on_project(&sy, project, &id).await?;
            note_fallback_board(origin, &board);
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
                let assets = save_body_assets(&sy, &md, &task.item_id).await;
                if !assets.is_empty() {
                    println!("Аттачи:");
                    for path in assets {
                        println!("  {path}");
                    }
                }
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
            let (board, task, origin) = syt::resolve_on_project(&sy, project, &id).await?;
            note_fallback_board(origin, &board);
            syt::edit(&sy, &board, &task, status.as_deref(), branch.as_deref(), pr.as_deref()).await?;
            println!("обновлено: {}", task.title);
            Ok(())
        }
        TaskSub::Comment { id, reply, stdin, text } => {
            let msg = guarded_comment_text(stdin, &text)?;
            let (board, task, origin) = syt::resolve_on_project(&sy, project, &id).await?;
            note_fallback_board(origin, &board);
            syt::comment(&sy, &board, &task, "AGENTS", &msg, reply.as_deref()).await?;
            println!("коммент отправлен: {}", truncate(&msg, 60));
            Ok(())
        }
        TaskSub::Thread { id } => {
            let (board, task, origin) = syt::resolve_on_project(&sy, project, &id).await?;
            note_fallback_board(origin, &board);
            let md = match syt::task_body_doc(&sy, &board, &task).await? {
                Some(doc) => sy.export_md_by_id(&doc).await.unwrap_or_default(),
                None => String::new(),
            };
            print_threads(&md);
            Ok(())
        }
        TaskSub::Close { id, note } => {
            let (board, task, origin) = syt::resolve_on_project(&sy, project, &id).await?;
            note_fallback_board(origin, &board);
            syt::close(&sy, &board, &task, note.as_deref()).await?;
            println!("закрыта: {}", task.title);
            Ok(())
        }
        TaskSub::Rm { id } => task_rm(&sy, project, &id).await,
        TaskSub::Clean { ids, yes, live } => task_clean(&sy, project, &ids, yes, live.as_deref()).await,
        TaskSub::Undo { journal, list } => {
            // --list сильнее позиционного (задаём список, имя не теряем — тест фиксирует)
            let name = if list { None } else { journal.as_deref() };
            task_undo(&sy, project, name).await
        }
        TaskSub::Body { id, replace, replace_section, md, stdin } => {
            let content = read_body_input(md.as_deref(), stdin)?;
            let (board, task, origin) = syt::resolve_on_project(&sy, project, &id).await?;
            note_fallback_board(origin, &board);
            let body_path = board.body_hpath(&task.title);
            let doc = syt::task_body_doc(&sy, &board, &task).await?;
            let doc = match doc {
                Some(doc_id) => {
                    write_body_in_place(&sy, &doc_id, &task.title, replace || replace_section.is_some(), replace_section.as_deref(), &content).await?;
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

// ——— guard: rm-вердикт, батч-снос, undo ———

/// Вердикт строки по независимым источникам: FS .sy тела, SQL по id,
/// док по тайтлу; для Draft с живым телом — экспорт против скелетов.
/// `chain` — цепочка doc-id доски (пути FS-проб).
async fn row_verdict(
    sy: &SiYuan,
    board: &syt::SyBoard,
    chain: &[String],
    task: &syt::SyTask,
) -> Result<Verdict> {
    let title_ids = syt::body_doc_by_title(sy, board, &task.title).await?;
    let fs = guard::probe_body_fs(sy, board, chain, task).await;
    let sql = guard::probe_body_sql(sy, task).await;
    let draft_body_export = match (fs, task.body_doc.as_deref()) {
        (Probe::Found, Some(doc)) if task.status.eq_ignore_ascii_case("draft") => {
            sy.export_md_by_id(doc).await.ok()
        }
        _ => None,
    };
    let evidence =
        guard::evidence_from_sources(task, title_ids.len(), fs, sql, draft_body_export.as_deref());
    Ok(guard::verdict(&evidence))
}

/// Строка undo-журнала: клетки + экспорт тела ДО сноса (None — тела нет).
async fn journal_row(sy: &SiYuan, board: &syt::SyBoard, task: &syt::SyTask) -> Result<JournalRow> {
    let body_md = match task.body_doc.as_deref() {
        Some(doc) => sy.export_md_by_id(doc).await.ok(),
        None => None,
    };
    let extra_text_cells = if task.schedule.is_empty() {
        Vec::new()
    } else {
        vec![("Schedule".to_string(), task.schedule.clone())]
    };
    Ok(JournalRow {
        item_id: task.item_id.clone(),
        title: task.title.clone(),
        status: task.status.clone(),
        branch: task.branch.clone(),
        pr: task.pr.clone(),
        extra_text_cells,
        body_doc: task.body_doc.clone(),
        body_hpath: board.body_hpath(&task.title),
        body_md,
    })
}

/// rm одной задачи под guard'ом: вердикт по источникам, отказ для живых
/// не-Draft (задекларированный контракт команды), write-ahead журнал.
async fn task_rm(sy: &SiYuan, project: &str, id: &str) -> Result<()> {
    let board = syt::find_board(sy, project).await?;
    let chain = guard::board_doc_chain(sy, &board).await?;
    let task = syt::resolve(sy, &board, id).await?;
    match row_verdict(sy, &board, &chain, &task).await? {
        Verdict::Divergent => bail!(
            "состояние строки «{}» неясно (FS/SQL-проба не состоялась) — rm отклонён, повтори позже",
            task.title
        ),
        Verdict::Live if !task.status.eq_ignore_ascii_case("draft") => bail!(
            "задача «{}» жива (статус «{}», тело существует): rm только для Draft-черновиков; \
             живую закрывай task close, owner-approved снос — task clean --yes --live",
            task.title, task.status
        ),
        _ => {}
    }
    let mut journal = Journal::new(project, "rm", &board);
    journal.rows.push(journal_row(sy, &board, &task).await?);
    let journal_path = journal.save(sy).await?;
    sy.mcp_tool(
        "database",
        json!({ "action": "item_remove", "id": board.av_id, "itemIDs": [task.item_id] }),
    )
    .await?;
    if let Some(doc) = syt::task_body_doc(sy, &board, &task).await? {
        let _ = sy.remove_doc(&doc).await;
    }
    println!("удалена: {} (undo-журнал: {journal_path})", task.title);
    Ok(())
}

/// Батч-снос строк под guard'ом: dry-run дефолт, гейт ack на весь батч,
/// write-ahead журнал, один item_remove массивом, пост-чек состава.
async fn task_clean(sy: &SiYuan, project: &str, ids: &str, yes: bool, live: Option<&str>) -> Result<()> {
    let frags: Vec<String> = ids
        .split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(String::from)
        .collect();
    if frags.is_empty() {
        bail!("--ids обязателен: кандидаты через запятую (row-id/префикс/подстрока)");
    }
    let board = syt::find_board(sy, project).await?;
    let chain = guard::board_doc_chain(sy, &board).await?;
    guard::control_probe_board(sy, &board, &chain).await?;
    let rows_before = syt::read_rows(sy, &board).await?;

    println!(
        "guard план{} — доска «{}» ({}), кандидатов: {}",
        if yes { " (--yes)" } else { " (dry-run)" },
        board.hpath,
        project,
        frags.len()
    );
    // один render и одна выборка саб-доков на весь батч: транспортный сбой
    // здесь — громкая ошибка, а не ярлык «фантом» на кандидате
    let subdoc_ids = syt::board_subdoc_ids(sy, &board).await?;
    let mut planned: Vec<(&str, syt::SyTask, Verdict)> = Vec::new();
    let mut seen_items: Vec<String> = Vec::new();
    for frag in &frags {
        match syt::match_needle(&rows_before, &subdoc_ids, frag) {
            syt::NeedleMatch::Found(task) => {
                if seen_items.contains(&task.item_id) {
                    println!("  [дубль        ] «{frag}» — та же строка, что и earlier фрагмент ({}), учтена один раз", task.item_id);
                    continue;
                }
                seen_items.push(task.item_id.clone());
                let verdict = row_verdict(sy, &board, &chain, &task).await?;
                let hint = if verdict.is_auto_removable() {
                    "снос с --yes (пустышка, владелец не нужен)"
                } else if verdict.is_removable() {
                    "снос с --yes (не-Draft: подтверждение владельца по процедуре)"
                } else if verdict.needs_live_ack() {
                    "защищена: снос только с --live"
                } else {
                    "не трогаю"
                };
                println!("  [{:<13}] «{}» (строка {}) — {}", verdict.label(), task.title, task.item_id, hint);
                planned.push((frag, task, verdict));
            }
            syt::NeedleMatch::NotFound => {
                println!("  [Phantom]     «{frag}» — не найдена в render, не трогаю")
            }
            syt::NeedleMatch::Ambiguous { reason, matches } => {
                println!("  [Ambiguous]   «{frag}» — неоднозначный {reason}: {matches} совпадений, уточни фрагмент; не трогаю")
            }
        }
    }

    let live_frags: Vec<String> = live
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(String::from)
        .collect();
    for frag in &live_frags {
        let matched = planned.iter().any(|(_, task, _)| guard::frag_matches(frag, task));
        if !matched {
            println!("предупреждение: --live «{frag}» не матчит ни одну строку плана (опечатка?)");
        }
    }

    let mut removable: Vec<(&syt::SyTask, Verdict)> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    for (_, task, verdict) in &planned {
        let acked = live_frags.iter().any(|f| guard::frag_matches(f, task));
        if verdict.is_removable() || (verdict.needs_live_ack() && acked) {
            removable.push((task, *verdict));
        } else if verdict.needs_live_ack() {
            refused.push(format!("«{}» — Live: нужен --live <фрагмент> или исключи из --ids", task.title));
        } else {
            refused.push(format!("«{}» — {}: не трогаю", task.title, verdict.label()));
        }
    }

    if !yes {
        println!("dry-run: мутаций нет. Исполнение — повтор с --yes (журнал и пост-чек включатся сами).");
        return Ok(());
    }
    if !refused.is_empty() {
        bail!("батч отклонён ЦЕЛИКОМ, ноль мутаций:\n  - {}", refused.join("\n  - "));
    }
    if removable.is_empty() {
        println!("сносить нечего: ни один кандидат не в вердиктах сноса");
        return Ok(());
    }

    // write-ahead: журнал обязан лежать в воркспейсе до первой мутации
    let mut journal = Journal::new(project, "clean", &board);
    for (task, _) in &removable {
        journal.rows.push(journal_row(sy, &board, task).await?);
    }
    let journal_path = journal.save(sy).await?;

    let item_ids: Vec<&str> = removable.iter().map(|(t, _)| t.item_id.as_str()).collect();
    sy.mcp_tool(
        "database",
        json!({ "action": "item_remove", "id": board.av_id, "itemIDs": item_ids }),
    )
    .await?;
    // тела: DraftSkeleton — пустой скелет в трэш; Live — тело НЕ трогается
    // (восстановимый сирота, практика спасения строк 04.11); Dead — тела нет
    for (task, verdict) in &removable {
        if *verdict == Verdict::DraftSkeleton {
            if let Some(doc) = task.body_doc.as_deref() {
                let _ = sy.remove_doc(doc).await;
            }
        }
    }

    let rows_after = syt::read_rows(sy, &board).await?;
    let before_titles: Vec<String> = rows_before.iter().map(|r| r.title.clone()).collect();
    let after_titles: Vec<String> = rows_after.iter().map(|r| r.title.clone()).collect();
    let removed_titles: Vec<String> = removable.iter().map(|(t, _)| t.title.clone()).collect();
    let extra = guard::extra_missing_titles(&before_titles, &after_titles, &removed_titles);
    if !extra.is_empty() {
        bail!(
            "ПОСТ-ЧЕК: с доски пропали строки ВНЕ плана ({:?}) — возможна потеря живых задач; \
             восстановление — task undo {} --project {project}",
            extra, journal_path
        );
    }
    println!(
        "снесено строк: {}; undo-журнал: {journal_path}; пост-чек: состав доски ок",
        removable.len()
    );
    Ok(())
}

/// Undo: список журналов или идемпотентное восстановление строк.
/// Тайтл уже на доске → no-op; id строк меняются — карта в выводе.
async fn task_undo(sy: &SiYuan, project: &str, journal_name: Option<&str>) -> Result<()> {
    let Some(name) = journal_name else {
        let journals = Journal::list(sy).await?;
        if journals.is_empty() {
            println!("undo-журналов нет ({})", guard::UNDO_DIR);
            return Ok(());
        }
        println!("undo-журналы ({}):", guard::UNDO_DIR);
        for (name, journal) in &journals {
            println!("  {name}  {} [{}, {} строк(а)]", journal.created, journal.kind, journal.rows.len());
        }
        println!("восстановление: task undo <имя> --project <проект журнала>");
        return Ok(());
    };
    let journal = Journal::load(sy, name).await?;
    if journal.project != project {
        bail!("журнал принадлежит проекту «{}», а --project «{project}»", journal.project);
    }
    let board = match journal.kind.as_str() {
        "cron-rm" => syt::find_board_cron(sy, project).await?,
        _ => syt::find_board(sy, project).await?,
    };
    if board.av_id != journal.av_id {
        bail!(
            "доска пересоздана (av {} ≠ {} из журнала) — авто-undo невозможен, восстанавливай вручную",
            board.av_id, journal.av_id
        );
    }
    let chain = guard::board_doc_chain(sy, &board).await?;
    guard::control_probe_board(sy, &board, &chain).await?;
    let rows = syt::read_rows(sy, &board).await?;
    let mut restored = 0usize;
    for row in &journal.rows {
        let present = rows
            .iter()
            .any(|r| syt::normalize_title(&r.title) == syt::normalize_title(&row.title));
        if present {
            println!("  пропущен (тайтл уже на доске): {}", row.title);
            continue;
        }
        let body_doc = restore_journal_body(sy, &board, row).await?;
        let item = if body_doc.is_empty() {
            sy.db_item_add(&board.av_id, &syt::path_safe(&row.title)).await?
        } else {
            sy.db_item_add_bound(&board.av_id, &body_doc).await?
        };
        restore_journal_cells(sy, &board, &journal.kind, &item, row).await?;
        println!(
            "  восстановлена: «{}» — строка {item} (была {}), тело {}",
            row.title,
            row.item_id,
            if body_doc.is_empty() { "—".to_string() } else { body_doc.clone() }
        );
        restored += 1;
    }
    println!(
        "undo: восстановлено {restored}/{}; id строк сменились — старые ссылки обнови (коммент «старый id → новый id»)",
        journal.rows.len()
    );
    Ok(())
}

/// Тело для восстановленной строки: живое переиспользуется, отсутствующее
/// пересоздаётся из журнала (docId меняется); без тела — detached-строка.
async fn restore_journal_body(sy: &SiYuan, board: &syt::SyBoard, row: &JournalRow) -> Result<String> {
    let Some(md) = row.body_md.as_deref() else {
        return Ok(String::new());
    };
    if let Some(doc) = row.body_doc.as_deref() {
        if syt::doc_block_exists(sy, doc).await {
            return Ok(doc.to_string());
        }
    }
    let ids = syt::body_doc_by_title(sy, board, &row.title).await?;
    match ids.as_slice() {
        [] => {
            let clean_md = markdown::strip_export_wrapper(md, &row.title);
            sy.create_doc_md(&board.notebook, &row.body_hpath, &clean_md).await
        }
        [only] => Ok(only.clone()),
        many => bail!(
            "несколько доков с заголовком «{}»: {} — вычисти дубли, потом повтори undo",
            row.title,
            many.join(", ")
        ),
    }
}

/// Клетки восстановленной строки: крон-доски — текстовые ячейки
/// (Schedule/Status), задачные — Status (mSelect) + Branch + PR (url).
async fn restore_journal_cells(
    sy: &SiYuan,
    board: &syt::SyBoard,
    kind: &str,
    item: &str,
    row: &JournalRow,
) -> Result<()> {
    if kind == "cron-rm" {
        // Status у кронов — mSelect (единый контур с task-путями); Schedule —
        // текстовая клетка из extra
        if !row.status.is_empty() {
            let sk = syt::status_key(sy, &board.av_id).await?;
            sy.db_item_set_status(&board.av_id, &sk, item, &row.status).await?;
        }
        if row.extra_text_cells.is_empty() {
            return Ok(());
        }
        let cells: Vec<(&str, &str)> = row
            .extra_text_cells
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        return set_text_cells(sy, &board.av_id, item, &cells).await;
    }
    if !row.status.is_empty() {
        let sk = syt::status_key(sy, &board.av_id).await?;
        sy.db_item_set_status(&board.av_id, &sk, item, &row.status).await?;
    }
    if !row.branch.is_empty() {
        set_text_cells(sy, &board.av_id, item, &[("Branch", row.branch.as_str())]).await?;
    }
    if row.pr.is_empty() {
        return Ok(());
    }
    let keys = sy.mcp_tool("database", json!({ "action": "keys", "id": board.av_id })).await?;
    let pr_key = keys
        .pointer("/data/keys")
        .or_else(|| keys.get("keys"))
        .and_then(|k| k.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|k| k.get("name").and_then(|n| n.as_str()) == Some("PR"))
                .and_then(|k| k.get("id").and_then(|i| i.as_str()).map(String::from))
        });
    match pr_key {
        Some(key) => {
            let value = syt::pr_cell_value(&row.pr)?;
            sy.mcp_tool(
                "database",
                json!({ "action": "item_update", "id": board.av_id, "keyID": key,
                        "itemID": item, "value": value }),
            )
            .await?;
            Ok(())
        }
        None => bail!("ключ PR не найден на доске — PR «{}» не восстановлен", row.pr),
    }
}

// ——— cron: строки доски «Кроны <proj>» ———

pub(crate) async fn cron_run(command: CronSub) -> Result<()> {
    let sy = SiYuan::from_env()?;
    match command {
        CronSub::List { project } => cron_list(&sy, &cron_project(project)).await,
        CronSub::Set { title, r#in, every, at, cron_expr, project, stdin } => {
            let schedule = cron_schedule(&r#in, &every, &at, &cron_expr)?;
            let prompt = if stdin { read_stdin()? } else { "<промпт крона>".into() };
            let proj = cron_project(project);
            let board = syt::find_board_cron(&sy, &proj).await?;
            // колонки — до мутаций: строка без Schedule демону не видна,
            // а тело/строка, созданные впустую, — сироты (репро 04.11)
            let keys = syt::board_keys(&sy, &board.av_id).await?;
            let cron_keys = syt::require_cron_keys(&keys, &board.hpath)?;
            // дубль-тайтл: живая строка или тело-сирота (дедуп как в task create)
            let canon = syt::normalize_title(&title);
            if syt::read_rows(&sy, &board).await?.iter().any(|r| syt::normalize_title(&r.title) == canon) {
                anyhow::bail!("крон «{title}» уже есть (живая строка на {})", board.hpath);
            }
            if !syt::body_doc_by_title(&sy, &board, &title).await?.is_empty() {
                anyhow::bail!("тело крона «{title}» уже есть");
            }
            // тело → bound-строка (клик по карточке открывает промпт) →
            // клетки → verify вставки в render (репро 04.11 08:27: success
            // item_add не гарантирует строку), паттерн task create
            let body_h = board.body_hpath(&title);
            let body = sy.create_doc_md(&board.notebook, &body_h, &cron_body_md(&title, &prompt)).await?;
            let mut item = insert_cron_row(&sy, &board, &cron_keys, &body, &schedule).await?;
            for attempt in 0..=1u8 {
                let rows = syt::read_rows(&sy, &board).await?;
                match syt::check_row_insert(&rows, &item, &title) {
                    syt::RowInsertCheck::Confirmed => {
                        println!("крон поставлен: {title}\n  расписание: {schedule}\n  строка: {item}\n  тело:   {body}");
                        return Ok(());
                    }
                    syt::RowInsertCheck::DuplicateTitle => bail!(
                        "строка «{title}» уже видна в render с другим itemID — вставка {item} не подтвердилась, вторую строку не создаю"
                    ),
                    syt::RowInsertCheck::Missing if attempt == 0 => {
                        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                        item = insert_cron_row(&sy, &board, &cron_keys, &body, &schedule).await?;
                    }
                    syt::RowInsertCheck::Missing => bail!(
                        "строка крона «{title}» не появилась в database после вставки и ретрая (item {item}, тело {body}); крон виден только как док"
                    ),
                }
            }
            unreachable!("цикл verify завершается return или bail")
        }
        CronSub::Pause { id, project } => {
            cron_set_status(&sy, &id, "paused", "остановлен", &cron_project(project)).await
        }
        CronSub::Resume { id, project } => {
            cron_set_status(&sy, &id, "active", "запущен", &cron_project(project)).await
        }
        CronSub::Rm { id, project } => {
            let proj = cron_project(project);
            let board = match syt::find_board_cron(&sy, &proj).await {
                Ok(b) => b,
                Err(_) => syt::find_board(&sy, &proj).await?,
            };
            let task = syt::resolve(&sy, &board, &id).await?;
            // undo-журнал до мутации: Schedule в extra-клетках, промпт — body_md
            let mut journal = Journal::new(&proj, "cron-rm", &board);
            journal.rows.push(journal_row(&sy, &board, &task).await?);
            let journal_path = journal.save(&sy).await?;
            sy.mcp_tool(
                "database",
                json!({ "action": "item_remove", "id": board.av_id, "itemIDs": [task.item_id] }),
            )
            .await?;
            if let Some(doc) = syt::task_body_doc(&sy, &board, &task).await? {
                let _ = sy.remove_doc(&doc).await;
            }
            println!("крон удалён: {} (undo-журнал: {journal_path})", task.title);
            Ok(())
        }
    }
}

/// Дефолтная доска кронов — «Кроны uwuwu».
fn cron_project(project: Option<String>) -> String {
    project.unwrap_or_else(|| "uwuwu".into())
}

fn cron_body_md(title: &str, prompt: &str) -> String {
    format!("# {title}\n\n```\n{prompt}\n```\n")
}

/// Bound-строка крона + клетки: Status через mSelect (единый контур с
/// task-путями), Schedule/Last Run текстом.
async fn insert_cron_row(
    sy: &SiYuan,
    board: &syt::SyBoard,
    keys: &syt::CronKeys,
    body: &str,
    schedule: &str,
) -> Result<String> {
    let item = sy.db_item_add_bound(&board.av_id, body).await?;
    sy.db_item_set_status(&board.av_id, &keys.status, &item, "active").await?;
    write_text_cell(sy, &board.av_id, &keys.schedule, &item, schedule).await?;
    write_text_cell(sy, &board.av_id, &keys.last_run, &item, "").await?;
    Ok(item)
}

/// Одна text-клетка (keyID уже валидирован — см. require_cron_keys).
async fn write_text_cell(sy: &SiYuan, av: &str, key: &str, item: &str, value: &str) -> Result<()> {
    sy.mcp_tool(
        "database",
        json!({ "action": "item_update", "id": av, "keyID": key, "itemID": item,
                "value": { "text": { "content": value } } }),
    )
    .await?;
    Ok(())
}

async fn cron_list(sy: &SiYuan, project: &str) -> Result<()> {
    let board = syt::find_board_cron(sy, project).await?;
    let rows = syt::read_rows(sy, &board).await?;
    print!("{}", format_cron_table(&rows));
    Ok(())
}

/// Таблица кронов: СТАТУС/СТРОКА/ЗАГОЛОВОК/SCHEDULE/LAST RUN — значения
/// всех колонок (раньше шапка обещала SCHEDULE/LAST RUN, а строка их не несла).
pub(crate) fn format_cron_table(rows: &[syt::SyTask]) -> String {
    let mut out = format!(
        "{:<10} {:<12} {:<28} {:<14} LAST RUN\n",
        "СТАТУС", "СТРОКА", "ЗАГОЛОВОК", "SCHEDULE"
    );
    for r in rows {
        out.push_str(&format!(
            "{:<10} {:<12} {:<28} {:<14} {}\n",
            r.status,
            &r.item_id[..r.item_id.len().min(12)],
            truncate(&r.title, 26),
            truncate(&r.schedule, 13),
            r.last_run
        ));
    }
    out
}

async fn cron_set_status(sy: &SiYuan, id: &str, status: &str, verb: &str, project: &str) -> Result<()> {
    let board = syt::find_board_cron(sy, project).await?;
    let task = syt::resolve(sy, &board, id).await?;
    let key = syt::status_key(sy, &board.av_id).await?;
    sy.db_item_set_status(&board.av_id, &key, &task.item_id, status).await?;
    println!("крон {verb}: {}", task.title);
    Ok(())
}

/// Текстовые клетки по именам колонок. Отсутствующая колонка — ошибка:
/// тихий скип оставлял строки без Schedule — демон не видел расписание
/// (репро 04.11, задача 20261104220156).
async fn set_text_cells(sy: &SiYuan, av: &str, item: &str, cells: &[(&str, &str)]) -> Result<()> {
    let keys = syt::board_keys(sy, av).await?;
    for (name, value) in cells {
        let Some(key) = keys.iter().find(|(n, _)| n == name).map(|(_, id)| id.as_str()) else {
            anyhow::bail!("колонка «{name}» не найдена на доске {av} — ячейка не записана");
        };
        write_text_cell(sy, av, key, item, value).await?;
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

/// Слаг → markdown дока (stdout печатает dispatch). Формы: `sofa/<post_id>`,
/// `<project>/<doc>`, `<topic>/<doc>`, слаг Daily, одиночный заголовок
/// (если однозначен) — резолв в `read_resolve`.
pub async fn read_markdown(slug: &str) -> Result<String> {
    if let Some(post_id) = slug.strip_prefix("sofa/") {
        let post = crate::sofa::get(post_id).await?;
        return Ok(format!("# {}\n\n{}\n", post.title, post.body));
    }
    let sy = SiYuan::from_env()?;
    let doc_id = read_resolve::resolve(&sy, slug).await?;
    sy.export_md_by_id(&doc_id).await
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

// ——— kernel: живые пробы ядра (one-shot, без ретраев) ———

/// Имена записей readDir: директории с хвостовым «/».
pub(crate) fn ls_lines(entries: &[Value]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|entry| {
            entry.get("name").and_then(|n| n.as_str()).map(|name| {
                if entry.get("isDir").and_then(|d| d.as_bool()).unwrap_or(false) {
                    format!("{name}/")
                } else {
                    name.to_string()
                }
            })
        })
        .collect()
}

/// doctor: read-only пробы деградации; свой exit-код поверх anyhow-механики
/// (0 чисто / 1 деградация / 2 пробы не выполнились) — консьюмеры exit-кода
/// (кроны, вотчдог) различают исходы.
async fn doctor_run(project: &str, since_min: u64) -> Result<()> {
    let sy = crate::siyuan::SiYuan::from_env_with_timeout(10)?;
    let outcomes = crate::siyuan_doctor::run(&sy, project, since_min).await;
    println!("{}", crate::siyuan_doctor::report(&outcomes));
    std::process::exit(crate::siyuan_doctor::doctor_exit_code(&outcomes));
}

async fn kernel_run(command: KernelSub, timeout_secs: u64) -> Result<()> {
    let sy = SiYuan::from_env_with_timeout(timeout_secs)?;
    match command {
        KernelSub::Ping => {
            sy.post_probe("/api/query/sql", json!({ "stmt": "select 1" })).await?;
            println!("alive: sql ok");
        }
        KernelSub::Sql { stmt } => {
            let envelope = sy.post_probe("/api/query/sql", json!({ "stmt": stmt })).await?;
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
            let envelope = sy.post_probe("/api/file/readDir", json!({ "path": path })).await?;
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
                eprintln!("(export пуст: 0 символов — док пуст, деградация индекса или неверный id)");
            }
            print!("{content}");
        }
        KernelSub::Render { av_id, block_id, page } => {
            crate::siyuan_id::ensure_node_id(&av_id, "av_id")?;
            crate::siyuan_id::ensure_node_id(&block_id, "block_id")?;
            let render = sy.db_render(&av_id, &block_id, page).await?;
            println!("{}", serde_json::to_string_pretty(&render)?);
        }
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
    doc_title: &str,
    rewrite: bool,
    section: Option<&str>,
    content: &str,
) -> Result<()> {
    if !rewrite {
        return sy.append_block_md(doc_id, content).await;
    }
    let existing_raw = sy.export_md_by_id(doc_id).await.unwrap_or_default();
    let existing = markdown::strip_export_wrapper(&existing_raw, doc_title);
    let new_md = match section {
        Some(section) => {
            let (md, edit) = replace_md_section(&existing, section, content);
            report_section_edit(section, edit);
            md
        }
        None => content.to_string(),
    };
    sy.overwrite_doc_md(doc_id, &new_md).await
}

/// Итог секционной правки: заменено (со счётчиком удалённых дублей) или добавлено.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SectionEdit {
    Replaced { duplicates_removed: usize },
    Appended,
}

/// Секция «## X» в markdown: содержимое первого вхождения заменяется,
/// заголовок пишется байт-в-байт (двойные пробелы и ^block-id живы); одноимённые
/// дубли удаляются вместе с контентом — хилинг заманженных прод-тел, счётчик
/// попадает в SectionEdit. Граница секции — заголовок уровня ≤ 2 вне кодового
/// фенса: gherkin-фенсы bug-шаблонов несут строки «# …», фенс-трекинг обязателен,
/// иначе новая граница резала бы их (регрессия относительно кода до уровня-aware).
/// Секция не найдена — append в конец с нормализованным заголовком; пустой ввод
/// очищает секцию (контракт); пустая строка перед следующим заголовком восстанавливается.
fn replace_md_section(md: &str, section: &str, content: &str) -> (String, SectionEdit) {
    let want = md_heading::normalize_heading_text(section);
    let mut out = String::new();
    let mut fenced = false;
    let mut dropping = false; // контент секции/дубля до границы не копируется
    let mut replaced = false;
    let mut duplicates_removed = 0usize;
    for line in md.lines() {
        if md_heading::toggles_fence(line) {
            fenced = !fenced;
            if !dropping {
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }
        if !fenced {
            if let Some((level, text)) = md_heading::heading_of(line) {
                if level == 2 && text == want {
                    if !replaced {
                        push_heading_block(&mut out, line, content);
                        replaced = true;
                    } else {
                        duplicates_removed += 1;
                    }
                    dropping = true;
                    continue;
                }
                if level <= 2 {
                    dropping = false;
                    push_blank_line(&mut out);
                    out.push_str(line);
                    out.push('\n');
                    continue;
                }
            }
        }
        if !dropping {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !replaced {
        let heading = format!("## {}", md_heading::normalize_heading_text(section));
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&heading);
        out.push('\n');
        if !content.trim().is_empty() {
            out.push_str(content.trim());
            out.push('\n');
        }
        return (out, SectionEdit::Appended);
    }
    (out, SectionEdit::Replaced { duplicates_removed })
}

/// Блок секции: заголовок исходной строкой + новый контент (пустой — очистка).
fn push_heading_block(out: &mut String, heading_line: &str, content: &str) {
    push_blank_line(out);
    out.push_str(heading_line);
    out.push('\n');
    if !content.trim().is_empty() {
        out.push_str(content.trim());
        out.push('\n');
    }
}

/// Пустая строка между блоками: не в начале вывода и не задвоенная.
fn push_blank_line(out: &mut String) {
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
}

/// Пометка секционной правки в stdout: append и хилинг дублей — не тихие.
fn report_section_edit(section: &str, edit: SectionEdit) {
    match edit {
        SectionEdit::Appended => println!("секция «{section}» не найдена — добавлена в конец"),
        SectionEdit::Replaced { duplicates_removed } if duplicates_removed > 0 => {
            println!("удалён дубль секции «{section}» ×{duplicates_removed}");
        }
        SectionEdit::Replaced { .. } => {}
    }
}

fn print_threads(md: &str) {
    println!("{}", collect_threads(md).trim_end());
}

/// Секции «## Комменты» тела: текст выдачи thread либо «комментов нет».
/// Матч нормализованный (двойные пробелы не теряют ответы), секция длится до
/// заголовка уровня ≤ 2 вне фенсов, дубли печатаются обе — read-путь не роняет.
fn collect_threads(md: &str) -> String {
    let mut out = String::new();
    let mut printing = false;
    let mut fenced = false;
    for line in md.lines() {
        if md_heading::toggles_fence(line) {
            fenced = !fenced;
            if printing {
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }
        if !fenced {
            if let Some((level, text)) = md_heading::heading_of(line) {
                if level <= 2 {
                    printing = level == 2 && text == "Комменты";
                    if printing {
                        out.push_str(line);
                        out.push('\n');
                    }
                    continue;
                }
            }
        }
        if printing {
            out.push_str(line);
            out.push('\n');
        }
    }
    if out.is_empty() {
        out.push_str("комментов нет\n");
    }
    out
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

fn collect_text(stdin: bool, text: &[String]) -> Result<String> {
    if stdin {
        read_stdin()
    } else {
        Ok(text.join(" "))
    }
}

/// Подкоманды task/cron: токен текста, совпадающий с именем, — почти наверняка
/// перепутанный вызов, а не контент (факт ses_e7b5a366: «list» ушёл юзеру
/// комментом вместо чтения треда). Токены сравниваются целиком: «get:» или
/// «get-задача» — контент.
const TASK_SUB_NAMES: &[&str] = &[
    "create", "list", "get", "edit", "comment", "thread", "close", "body", "rm", "clean", "undo",
    "set", "pause", "resume",
];

/// Токен выглядит как флаг: `--…` всегда; одиночный дефис — только перед
/// буквой (`-n`, `-Prb`), чтобы не трогать markdown-буллеты («- пункт») и
/// отрицательные числа («-5»).
fn looks_like_flag(token: &str) -> bool {
    if token.starts_with("--") {
        return true;
    }
    token
        .strip_prefix('-')
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| c.is_alphabetic())
}

/// Текст коммента + гвард до сети: подозрительный позиционный текст — отказ
/// с подсказкой вместо тихой отправки юзеру. Сама валидация —
/// validate_comment_text (тестируется на строках, stdin не нужен).
fn guarded_comment_text(stdin: bool, text: &[String]) -> Result<String> {
    let msg = collect_text(stdin, text)?;
    validate_comment_text(stdin, &msg)?;
    Ok(msg)
}

/// Гвард текста коммента: пустой — отказ в обоих источниках; из позиционных
/// токенов отклоняются имена подкоманд и флагоподобные токены. stdin не
/// гвардится на подкоманды и флаги: явное намерение, глотание туда невозможно.
fn validate_comment_text(stdin: bool, msg: &str) -> Result<()> {
    if msg.trim().is_empty() {
        bail!("пустой текст коммента — передай --stdin или текст позиционным аргументом");
    }
    if stdin {
        return Ok(());
    }
    let tokens: Vec<&str> = msg.split_whitespace().collect();
    if let Some(first) = tokens.first().filter(|t| TASK_SUB_NAMES.contains(t)) {
        bail!(
            "«{first}» — имя подкоманды task/cron, а не текст коммента \
             (читать комменты: task get/thread <id>; писать: task comment <id> \"текст\" или --stdin)"
        );
    }
    if let Some(flag) = tokens.iter().find(|t| looks_like_flag(t)) {
        bail!(
            "токен «{flag}» похож на флаг: после текста коммента флаги глотаются как текст \
             (trailing_var_arg) — пиши флаги до <id> или используй --stdin"
        );
    }
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Asset-ссылки markdown-тела `](assets/…)` в порядке появления без дублей:
/// путь до `)` или пробела, percent-decode %XX (SiYuan кодирует пробелы и
/// юникод в именах аттачей). HTML-формы `<img src=…>` в экспорте не встречались.
fn collect_asset_refs(md: &str) -> Vec<String> {
    const PREFIX: &str = "](assets/";
    let mut out = Vec::new();
    let mut rest = md;
    while let Some(pos) = rest.find(PREFIX) {
        rest = &rest[pos + PREFIX.len()..];
        let raw: String = rest
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != ')')
            .collect();
        rest = rest.strip_prefix(raw.as_str()).unwrap_or(rest);
        let key = percent_decode(&raw);
        if !key.is_empty() && !out.contains(&key) {
            out.push(key);
        }
    }
    out
}

/// %XX → байты (имена аттачей SiYuan percent-кодирует); невалидные
/// последовательности копируются как есть.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Ключ asset-ссылки безопасен: ни один сегмент пути не пуст, «.» или «..» —
/// такие ключи («assets/../../etc») не скачиваются вовсе, до сетевого вызова.
fn asset_key_is_safe(key: &str) -> bool {
    key.split('/').all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

/// Выгрузить asset-аттачи тела в /tmp/uwuwu-task-<row-id>/ (полный row-id —
/// контракт AGENTS.md; таймстамп-голова давала бы коллизии каталогов задач
/// одного дня). Оригинальные имена файлов; максимум 10; сбой одного аттача —
/// тихий пропуск (остаёмся на ссылках, как в аффин-контракте). Ключи с
/// точечными сегментами (../) не скачиваются вовсе.
async fn save_body_assets(sy: &SiYuan, md: &str, row_id: &str) -> Vec<String> {
    const MAX_ASSETS: usize = 10;
    let refs = collect_asset_refs(md);
    let safe: Vec<&String> = refs.iter().filter(|key| asset_key_is_safe(key)).collect();
    if safe.is_empty() {
        return Vec::new();
    }
    let dir = format!("/tmp/uwuwu-task-{row_id}");
    if std::fs::create_dir_all(&dir).is_err() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for key in safe.into_iter().take(MAX_ASSETS) {
        let Ok(bytes) = sy.download_file(&format!("assets/{key}")).await else {
            continue;
        };
        // ядро молча отдаёт 200+0 байт для отсутствующего файла (контракт 3.8.9):
        // пустой ответ = файла нет, локальный пустыш не пишем
        if bytes.is_empty() {
            continue;
        }
        let name = key.rsplit('/').next().unwrap_or(key);
        let path = format!("{dir}/{name}");
        if std::fs::write(&path, &bytes).is_ok() {
            out.push(path);
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::asset_key_is_safe;
    use super::collect_asset_refs;
    use super::collect_threads;
    use super::replace_md_section;
    use super::save_body_assets;
    use super::validate_comment_text;
    use super::SectionEdit;
    use super::task_run;
    use crate::cmd::cli::TaskSub;
    use crate::siyuan::SiYuan;
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
        let mut view = render_rows(with_binding);
        view["rowCount"] = serde_json::json!(1);
        let payload = serde_json::json!({ "action": "render", "data": { "view": view } });
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
        mock_siyuan_with_export(title_doc_ids, with_binding, String::new()).await
    }

    /// Расширенный мок: с экспортом дока (обёртка frontmatter + имя) для
    /// rewrite-пути (replace/--replace-section).
    async fn mock_siyuan_with_export(
        title_doc_ids: Vec<String>,
        with_binding: bool,
        export_content: String,
    ) -> (MockServer, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
        let server = MockServer::start().await;
        let create_calls = Arc::new(AtomicUsize::new(0));
        let append_bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

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
                let data = if hpath == "/uwuwu/Задачи uwuwu" {
                    serde_json::json!([BOARD_DOC])
                } else if hpath == "/uwuwu/Задачи uwuwu/таск" {
                    serde_json::json!(ids)
                } else {
                    // прочие пути (крон-доски и пр.) в этом сценарии не существуют
                    serde_json::json!([])
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

        Mock::given(method("POST"))
            .and(path("/api/export/exportMdContent"))
            .respond_with(move |_req: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_string(
                    serde_json::json!({ "code": 0, "data": { "hPath": "/uwuwu/Задачи uwuwu/таск", "content": export_content } })
                        .to_string(),
                )
            })
            .mount(&server)
            .await;

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
                    if body.contains("\"block\"") && body.contains("\"get\"") {
                        // doc_block_exists: живой bound-док → NodeDocument
                        ResponseTemplate::new(200).set_body_string(
                            format!(
                                r#"{{"jsonrpc":"2.0","id":2,"result":{{"content":[{{"type":"text","text":"ID: {BOUND_DOC}\nType: NodeDocument\nHPath: /uwuwu/Задачи uwuwu/таск"}}]}}}}"#
                            ),
                        )
                    } else if body.contains("\"block\"") && body.contains("get_children") {
                        // overwrite: док пуст → doc_content_is_empty → append
                        ResponseTemplate::new(200).set_body_string(
                            r#"{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":""}]}}"#,
                        )
                    } else {
                        ResponseTemplate::new(200).set_body_string(mcp_render_response(with_binding))
                    }
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
        {
            let appends = append_bodies.lock().unwrap();
            assert_eq!(appends.len(), 1, "один appendBlock в привязанный док");
            assert!(appends[0].contains("маркер-контент"), "контент в appendBlock");
            assert!(
                appends[0].contains(&format!("\"parentID\":\"{BOUND_DOC}\"")),
                "append в привязанный док, не в сироту"
            );
        }
    }

    /// Rewrite-путь (--replace-section): existing-экспорт несёт обёртку
    /// (frontmatter + рендер-строка имени дока) — после перезаписи НИ имя,
    /// НИ frontmatter не реимпортируются реальными блоками (симптом
    /// «--replace-section множит заголовки»).
    #[tokio::test]
    #[serial_test::serial]
    async fn replace_section_rewrites_without_name_and_frontmatter_blocks() {
        let export = "---\ntitle: таск\n---\n\n# таск\n\n## Проверка\nстарое тело секции\n".to_string();
        let (server, create_calls, append_bodies) =
            mock_siyuan_with_export(vec![BOUND_DOC.to_string()], true, export).await;
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
        let md = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(md.path(), "новое содержимое секции\n").unwrap();
        task_run(
            "uwuwu",
            TaskSub::Body {
                id: "таск".into(),
                stdin: false,
                md: Some(md.path().to_string_lossy().into_owned()),
                replace: false,
                replace_section: Some("Проверка".into()),
            },
        )
        .await
        .unwrap();

        assert_eq!(create_calls.load(Ordering::SeqCst), 0);
        {
            let appends = append_bodies.lock().unwrap();
            assert_eq!(appends.len(), 1, "один appendBlock пересборки");
            let payload = &appends[0];
            assert!(payload.contains("новое содержимое секции"), "новая секция в appendBlock");
            assert!(!payload.contains("# таск"), "имя дока не реимпортируется блоком");
            assert!(!payload.contains("title: таск"), "frontmatter не реимпортируется блоком");
            assert!(!payload.contains("старое тело секции"), "старая секция заменена");
        }
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
    fn replace_md_section_replaces_body_keeps_heading() {
        let md = "## А\nстарое\n\n## Б\nвторое\n";
        let (out, edit) = replace_md_section(md, "А", "новое содержимое\n");
        assert!(out.contains("## А\nновое содержимое"));
        assert!(out.contains("## Б\nвторое"));
        assert_eq!(edit, SectionEdit::Replaced { duplicates_removed: 0 });
    }

    #[test]
    fn replace_md_section_appends_missing_section() {
        let md = "## А\nпервое\n";
        let (out, edit) = replace_md_section(md, "Новая", "контент");
        assert!(out.contains("## Новая\nконтент"));
        assert!(out.contains("## А\nпервое"));
        assert_eq!(edit, SectionEdit::Appended);
    }

    #[test]
    fn replaces_first_and_removes_duplicate_sections() {
        let md = "# Таск\n\n## Причина\nпервое вхождение\n\n## Причина\nвторое вхождение\n\n## Фикс\nправка\n";
        let (out, edit) = replace_md_section(md, "Причина", "новый механизм\n");
        assert_eq!(out.matches("## Причина").count(), 1, "секция остаётся одна");
        assert!(!out.contains("первое вхождение"), "старый контент первого удалён");
        assert!(!out.contains("второе вхождение"), "контент дубля удалён");
        assert!(out.contains("## Фикс\nправка"), "соседняя секция не тронута");
        assert_eq!(edit, SectionEdit::Replaced { duplicates_removed: 1 });
    }

    #[test]
    fn ends_section_at_higher_level_heading() {
        let md = "## А\nстарое\n# Заголовок H1\nхвост тайтла\n\n## Б\nвторое\n";
        let (out, _) = replace_md_section(md, "А", "новое\n");
        assert!(out.contains("# Заголовок H1"), "h1 переживает replace");
        assert!(out.contains("хвост тайтла"), "контент после h1 переживает replace");
        assert!(out.contains("## Б\nвторое"));
    }

    #[test]
    fn h3_stays_inside_section() {
        let md = "## А\nввод\n### Подсекция\nдетали подсекции\n\n## Б\nвторое\n### Б-под\nб-детали\n";
        let (out, _) = replace_md_section(md, "А", "новое\n");
        assert!(
            !out.contains("детали подсекции"),
            "h3 не рвёт диапазон: контент под h3 заменяется вместе с секцией"
        );
        assert!(out.contains("### Б-под\nб-детали"), "h3 чужой секции не тронут");
        assert!(out.contains("## Б\nвторое"));
    }

    #[test]
    fn normalizes_heading_and_argument() {
        let md = "##  Постановка\nстарое\n";
        let (out, edit) = replace_md_section(md, " Постановка ", "новое\n");
        assert!(out.contains("новое"), "матч по нормализованному тексту");
        assert_eq!(out.matches("##").count(), 1, "без append-дубля секции");
        assert_eq!(edit, SectionEdit::Replaced { duplicates_removed: 0 });
    }

    #[test]
    fn heading_preserved_byte_for_byte() {
        let md = "##  Причина ^20261104120000-ab12cd\nстарое\n\n## Б\nвторое\n";
        let (out, _) = replace_md_section(md, "Причина", "новое\n");
        assert!(
            out.contains("##  Причина ^20261104120000-ab12cd\nновое"),
            "заматченный заголовок пишется исходной строкой (пробелы и ^id живы)"
        );
        assert_eq!(out.matches("##").count(), 2, "без append-дубля");
    }

    #[test]
    fn matches_heading_with_block_id_suffix() {
        let md = "## Причина ^20261104120000-ab12cd\nстарое\n\n## Б\nвторое\n";
        let (out, edit) = replace_md_section(md, "Причина", "новое\n");
        assert!(out.contains("новое"), "суффикс ^id не мешает матчу");
        assert_eq!(edit, SectionEdit::Replaced { duplicates_removed: 0 });
    }

    #[test]
    fn fenced_h2_does_not_bound_section() {
        let md = "## А\nстарое\n```md\n## Фейк внутри фенса\n```\nхвост секции\n\n## Б\nвторое\n";
        let (out, _) = replace_md_section(md, "А", "новое\n");
        assert!(!out.contains("Фейк"), "h2 внутри фенса — контент секции, заменяется");
        assert!(!out.contains("хвост секции"), "секция длится сквозь фенс");
        assert!(out.contains("## Б\nвторое"));
    }

    #[test]
    fn hash_line_inside_fence_survives() {
        let md = "## Сценарий (BDD)\nстарое\n```gherkin\nСценарий: x\n  Дано y\n  Тогда z  # факт\n```\n\n## Б\nвторое\n";
        let (out, _) = replace_md_section(md, "Сценарий (BDD)", "новое\n");
        assert!(out.contains("## Б\nвторое"), "граница секции — настоящий заголовок Б");
        assert_eq!(out.matches("## Б").count(), 1, "секция Б не задвоена");
        assert!(!out.contains("# факт"), "старый контент секции заменён целиком");
    }

    #[test]
    fn empty_content_clears_section() {
        let md = "## А\nстарое\n\n## Б\nвторое\n";
        let (out, _) = replace_md_section(md, "А", "");
        assert!(out.contains("## А"), "заголовок сохранён");
        assert!(!out.contains("старое"), "контент очищен пустым вводом");
        assert!(out.contains("## Б\nвторое"));
    }

    #[test]
    fn blank_line_restored_before_next_heading() {
        let md = "## А\nстарое\n\n## Б\nвторое\n";
        let (out, _) = replace_md_section(md, "А", "новое\n");
        assert!(out.contains("новое\n\n## Б"), "пустая строка между секциями восстановлена");
    }

    #[test]
    fn collect_threads_prints_normalized_section() {
        let md = "## Постановка\nтело\n\n##  Комменты\n- **AGENTS**: вопрос\n";
        let out = collect_threads(md);
        assert!(out.contains("##  Комменты"), "заголовок с двойным пробелом матчится");
        assert!(out.contains("- **AGENTS**: вопрос"));
        assert!(!out.contains("тело"));
    }

    #[test]
    fn collect_threads_ends_at_h1_boundary() {
        let md = "## Комменты\n- коммент\n# Тег\nтекст после тега\n";
        let out = collect_threads(md);
        assert!(out.contains("- коммент"));
        assert!(!out.contains("# Тег"), "h1 закрывает секцию");
        assert!(!out.contains("текст после тега"));
    }

    #[test]
    fn collect_threads_prints_both_duplicate_sections() {
        let md = "## Комменты\n- первый\n\n## Комменты\n- второй\n";
        let out = collect_threads(md);
        assert!(out.contains("- первый"));
        assert!(out.contains("- второй"), "дубли секций печатаются обе — read-путь ничего не роняет");
    }

    #[test]
    fn collect_threads_prints_hash_tag_inside_section() {
        let md = "## Комменты\n- коммент\n#bug\n- коммент после тега\n";
        let out = collect_threads(md);
        assert!(out.contains("#bug"), "#bug без пробела — не заголовок, секцию не закрывает");
        assert!(out.contains("- коммент после тега"));
    }

    #[test]
    fn collect_threads_reports_empty() {
        assert_eq!(collect_threads("## А\nтело\n").trim_end(), "комментов нет");
    }

    #[test]
    fn comment_guard_rejects_subcommand_name_as_text() {
        let err = validate_comment_text(false, "list").expect_err("«list» — подкоманда, не текст");
        assert!(err.to_string().contains("подкоманд"), "суть отказа: {err}");
        assert!(err.to_string().contains("task get/thread"), "подсказка чтения: {err}");
    }

    #[test]
    fn comment_guard_rejects_flag_swallowed_after_text() {
        let err = validate_comment_text(false, "текст --project x")
            .expect_err("флаг после текста глотается trailing_var_arg");
        assert!(err.to_string().contains("флаг"), "суть отказа: {err}");
        assert!(err.to_string().contains("--stdin"), "обход указан: {err}");
    }

    #[test]
    fn comment_guard_rejects_dash_prefixed_word() {
        let err = validate_comment_text(false, "-n").expect_err("-n выглядит как флаг");
        assert!(err.to_string().contains("флаг"), "суть отказа: {err}");
    }

    #[test]
    fn comment_guard_allows_markdown_bullets() {
        validate_comment_text(false, "- пункт один\n- ещё пункт").expect("markdown-буллет — контент, не флаг");
    }

    #[test]
    fn comment_guard_allows_minus_number_and_lone_dash() {
        validate_comment_text(false, "-5 баллов").expect("отрицательное число — контент");
        validate_comment_text(false, "-").expect("одиночный дефис — контент");
    }

    #[test]
    fn comment_guard_rejects_empty_text() {
        let err = validate_comment_text(false, "").expect_err("пустой коммент — всегда баг");
        assert!(err.to_string().contains("пустой"), "суть отказа: {err}");
        validate_comment_text(true, "   ").expect_err("пустой stdin-коммент тоже отказан");
    }

    #[test]
    fn comment_guard_skips_stdin_source() {
        validate_comment_text(true, "--не-флаг-а-текст").expect("stdin — явное намерение, не гвардим");
        validate_comment_text(true, "list из пайпа — тоже контент").expect("stdin: подкоманды не гвардятся");
    }

    // ——— порядок диспетчера: гвард текста раньше bail по --project ———

    /// `task comment <id> …` при пустом --project: оба пути падают до
    /// SiYuan::from_env (гвард/байл — первые строки task_run), сети нет.
    fn comment_command(stdin: bool, text: &[&str]) -> TaskSub {
        TaskSub::Comment {
            id: "20261104072048-4n1rwdt".into(),
            reply: None,
            stdin,
            text: text.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Факт AGENTS-коммента задачи (крон-сессия 04.11, ×3): `task comment <id>
    /// "текст" --project X` — флаг проглочен trailing_var_arg'ом → project=None →
    /// bail «--project обязателен» маскировал причину под «флаг не передан».
    /// Гвард должен стрелять РАНЬШЕ bail по --project.
    #[tokio::test]
    async fn comment_guard_fires_before_project_bail_on_swallowed_flag() {
        let err = task_run("", comment_command(false, &["текст", "--project", "X"]))
            .await
            .expect_err("флаг в позиционном тексте — отказ гварда");
        let msg = err.to_string();
        assert!(msg.contains("похож на флаг"), "реальная причина в ошибке: {msg}");
        assert!(!msg.contains("обязателен"), "маскировка «флаг не передан» недопустима: {msg}");
    }

    /// Пинну приоритета (замечание ревью плана): подкоманда-как-текст при
    /// ОТСУТСТВУЮЩЕМ --project — тоже гвард раньше bail («list» вместо
    /// «--project обязателен»).
    #[tokio::test]
    async fn comment_guard_fires_before_project_bail_on_subcommand_token() {
        let err = task_run("", comment_command(false, &["list"]))
            .await
            .expect_err("подкоманда-как-текст — отказ гварда");
        let msg = err.to_string();
        assert!(msg.contains("подкоманд"), "сообщение гварда: {msg}");
        assert!(!msg.contains("обязателен"), "приоритет ошибок сменился обратно: {msg}");
    }

    /// Контроль: чистый позиционный текст при пустом --project — прежний bail
    /// (проверка --project не размыта гвардом).
    #[tokio::test]
    async fn clean_comment_text_still_bails_on_missing_project() {
        let err = task_run("", comment_command(false, &["текст"]))
            .await
            .expect_err("--project обязателен для чистого текста");
        assert!(err.to_string().contains("обязателен"), "{err}");
    }

    #[test]
    fn collect_asset_refs_extracts_unique_assets_in_order() {
        let md = "![а](assets/first.png)\nтекст [файл](assets/second.htm) ![а](assets/first.png)\n";
        assert_eq!(collect_asset_refs(md), vec!["first.png", "second.htm"]);
    }

    #[test]
    fn collect_asset_refs_percent_decodes_names() {
        let md = "![ф](assets/%D0%B8%D0%BC%D1%8F%20я.png)\n";
        assert_eq!(collect_asset_refs(md), vec!["имя я.png"]);
    }

    #[test]
    fn collect_asset_refs_ignores_non_asset_links() {
        let md = "[сайт](https://example.com/x.png) [внутр](/Projects/doc) ![п](assets/ok.png)\n";
        assert_eq!(collect_asset_refs(md), vec!["ok.png"]);
    }

    #[tokio::test]
    async fn save_body_assets_downloads_with_original_names_and_skips_missing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/file/getFile"))
            .respond_with(|req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let path = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
                if path == "data/assets/assets-self-test/img.png" {
                    ResponseTemplate::new(200).set_body_bytes(b"png-bytes".to_vec())
                } else {
                    ResponseTemplate::new(404).set_body_string(r#"{"code":404,"msg":"path does not exist"}"#)
                }
            })
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        const ROW: &str = "20261105090000-assette1";
        let md = "![и](assets/assets-self-test/img.png) ![нет](assets/assets-self-test/gone.png)\n";
        let saved = save_body_assets(&sy, md, ROW).await;

        assert_eq!(saved.len(), 1, "доступный аттач сохранён, Missing — тихо пропущен: {saved:?}");
        assert!(saved[0].ends_with("/img.png"), "оригинальное имя файла: {saved:?}");
        let bytes = std::fs::read(&saved[0]).expect("файл записан");
        assert_eq!(bytes, b"png-bytes");
        std::fs::remove_dir_all(format!("/tmp/uwuwu-task-{ROW}")).expect("тест прибрал за себя");
    }

    #[tokio::test]
    async fn save_body_assets_never_downloads_traversal_keys() {
        let server = MockServer::start().await;
        let file_calls = Arc::new(AtomicUsize::new(0));
        let calls = file_calls.clone();
        Mock::given(method("POST"))
            .and(path("/api/file/getFile"))
            .respond_with(move |req: &wiremock::Request| {
                calls.fetch_add(1, Ordering::SeqCst);
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let path = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
                if path == "data/assets/ok.png" {
                    ResponseTemplate::new(200).set_body_bytes(b"png-bytes".to_vec())
                } else {
                    ResponseTemplate::new(404).set_body_string(r#"{"code":404,"msg":"path does not exist"}"#)
                }
            })
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        const ROW: &str = "20261105090000-travers";
        let md = "![x](assets/../../etc) ![y](assets/..) ![z](assets/./x) ![ok](assets/ok.png)\n";
        let saved = save_body_assets(&sy, md, ROW).await;

        assert_eq!(saved.len(), 1, "только безопасный ключ скачан: {saved:?}");
        assert_eq!(
            file_calls.load(Ordering::SeqCst),
            1,
            "точечные сегменты (../ и ./) не скачиваются вовсе — один getFile за ok.png"
        );
        std::fs::remove_dir_all(format!("/tmp/uwuwu-task-{ROW}")).expect("тест прибрал за себя");
    }

    #[test]
    fn asset_key_is_safe_rejects_any_dot_segment() {
        assert!(!asset_key_is_safe("../../etc"));
        assert!(!asset_key_is_safe("sub/./x"));
        assert!(!asset_key_is_safe("a/../b"));
        assert!(!asset_key_is_safe(""));
        assert!(asset_key_is_safe("img.png"));
        assert!(asset_key_is_safe("sub/dir/file.htm"));
    }
}
