//! Handlers всех команд uwuwu-cli на SiYuan-бекенде (единственный).

use super::read_resolve;
use crate::cmd::cli::{Commands, CronSub, KernelSub, PersonSub, SemanticSub, TaskSub};
use crate::markdown;
use crate::md_heading;
use crate::siyuan_person as person;
use crate::siyuan_person::person_target;
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
        Commands::Search { query, limit, sofa, threshold } => {
            search_run(&query, limit, threshold, sofa).await
        }
        Commands::Semantic { command } => semantic_run(command).await,
        Commands::Read { slug } => {
            let md = read_markdown(&slug).await?;
            print!("{md}");
            Ok(())
        }
        Commands::Blob { key, out } => blob_run(&key, out).await,
        Commands::Person { command } => person_run(command).await,
        Commands::Projects => projects_run().await,
        Commands::Shot { .. } => Err(anyhow!(
            "shot: не портирован на SiYuan (v1) — headless chromium на http://127.0.0.1:6806 с Access Auth Code"
        )),
        Commands::Kernel { command, timeout } => kernel_run(command, timeout).await,
        Commands::Doctor { project, since } => doctor_run(&project, since).await,
    }
}

pub(crate) async fn task_run(project: &str, command: TaskSub) -> Result<()> {
    // dry-run ветка раньше project-проверки и from_env: скелет тела от проекта
    // не зависит, осмотр — read-only и работает без доступа к ядру (ноль запросов)
    if let TaskSub::Create {
        title: _,
        r#type: ty,
        stdin,
        attach,
        dry_run: true,
    } = command
    {
        if !attach.is_empty() {
            anyhow::bail!("create --dry-run не аттачит: скелет печатается без записей в воркспейс — убери --attach");
        }
        let ctx = if stdin { read_stdin()? } else { String::new() };
        println!(
            "{}",
            syt::body_template(ty.as_deref().unwrap_or("dev"), &ctx)
        );
        return Ok(());
    }
    // Гвард позиционного текста коммента ДО bail по --project: иначе флаг,
    // проглоченный trailing_var_arg'ом (`task comment <id> "текст" --project X`
    // → project=None), маскируется под «флаг не передан» (крон-сессия 04.11).
    // stdin здесь НЕ читается — его читает ровно одна ветка ниже.
    if let TaskSub::Comment { stdin, text, .. } = &command {
        pre_guard_positional_comment(*stdin, text)?;
    }
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
        TaskSub::Create {
            title,
            r#type: ty,
            stdin,
            attach,
            ..
        } => {
            let attach_files = validate_attach_files(&attach)?;
            let ctx = if stdin { read_stdin()? } else { String::new() };
            // пред-чек дедупа тайтла ДО uploads: ретрай с живой строкой не
            // должен грузить ассеты впустую (внутренний чек create остаётся
            // авторитетным — гонка между чеками ловится им же)
            if !attach_files.is_empty() {
                let board = syt::find_board(&sy, project).await?;
                let rows = syt::read_rows(&sy, &board).await?;
                let canon = syt::normalize_title(&title);
                if rows.iter().any(|r| syt::normalize_title(&r.title) == canon) {
                    anyhow::bail!("живая строка с тайтлом «{title}» уже есть на доске — дубль");
                }
                if !syt::body_doc_by_title(&sy, &board, &title)
                    .await?
                    .is_empty()
                {
                    anyhow::bail!("тело задачи «{title}» уже есть");
                }
            }
            // upload всех до мутаций доски: сбой не оставляет задачу без скрина;
            // мусор-ассеты возможен только при падении между upload и create
            let mut uploaded = Vec::new();
            for file in attach_files {
                uploaded.push(sy.upload_asset(file).await?);
            }
            let (item, body) =
                syt::create(&sy, project, &title, ty.as_deref().unwrap_or("dev"), &ctx).await?;
            if !uploaded.is_empty() {
                let refs = attach_refs_md(&uploaded);
                sy.append_block_md(&body, &refs).await.with_context(|| {
                    format!(
                        "задача создана (строка {item}, тело {body}), но референсы не дописаны — \
                         доложи: task body {item} --attach <файлы>"
                    )
                })?;
            }
            println!("создана: {title}\n  строка: {item}\n  тело:   {body}");
            if !uploaded.is_empty() {
                let paths: Vec<&str> = uploaded.iter().map(|a| a.asset_path.as_str()).collect();
                println!("  ассеты: {}", paths.join(", "));
            }
            Ok(())
        }
        TaskSub::Edit {
            id,
            status,
            title,
            pr,
            branch,
        } => {
            let (board, task, origin) = syt::resolve_on_project(&sy, project, &id).await?;
            note_fallback_board(origin, &board);
            // валидация PR до rename: без частичных обновлений (тайтл сменился,
            // а колонки нет — недопустимый разрыв)
            pr.as_deref().map(syt::pr_cell_value).transpose()?;
            let final_title = match title.as_deref() {
                Some(raw) => Some(syt::edit_title(&sy, &board, &task, raw).await?),
                None => None,
            };
            syt::edit(
                &sy,
                &board,
                &task,
                status.as_deref(),
                branch.as_deref(),
                pr.as_deref(),
            )
            .await?;
            println!(
                "обновлено: {}",
                final_title.as_deref().unwrap_or(&task.title)
            );
            Ok(())
        }
        TaskSub::Comment {
            id,
            reply,
            stdin,
            text,
        } => {
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
        TaskSub::Clean { ids, yes, live } => {
            task_clean(&sy, project, &ids, yes, live.as_deref()).await
        }
        TaskSub::Undo { journal, list } => {
            // --list сильнее позиционного (задаём список, имя не теряем — тест фиксирует)
            let name = if list { None } else { journal.as_deref() };
            task_undo(&sy, project, name).await
        }
        TaskSub::Body {
            id,
            replace,
            replace_section,
            md,
            stdin,
            attach,
        } => {
            let attach_files = validate_attach_files(&attach)?;
            let content = read_body_input(md.as_deref(), stdin, !attach_files.is_empty())?;
            let (board, task, origin) = syt::resolve_on_project(&sy, project, &id).await?;
            note_fallback_board(origin, &board);
            let body_path = board.body_hpath(&task.title);
            // upload до записи тела: сбой не оставляет тело без референсов
            let mut uploaded = Vec::new();
            for file in attach_files {
                uploaded.push(sy.upload_asset(file).await?);
            }
            let doc = syt::task_body_doc(&sy, &board, &task).await?;
            let doc = match doc {
                Some(doc_id) => {
                    // attach-only вызов (пустой контент без rewrite) не делает
                    // пустой appendBlock: опора на недокументированную реакцию
                    // ядра на пустой data — референсы дописываются сами
                    if replace || replace_section.is_some() || !content.trim().is_empty() {
                        write_body_in_place(
                            &sy,
                            &doc_id,
                            &task.title,
                            replace || replace_section.is_some(),
                            replace_section.as_deref(),
                            &content,
                        )
                        .await?;
                    }
                    doc_id
                }
                None => {
                    // тела нет вовсе: создаю по заголовку (не привязываю — itemID строки неприкосновенен)
                    let initial = if replace || replace_section.is_some() {
                        content.clone()
                    } else {
                        content.trim_start().to_string()
                    };
                    sy.create_doc_md(&board.notebook, &body_path, &initial)
                        .await?
                }
            };
            if !uploaded.is_empty() {
                let refs = attach_refs_md(&uploaded);
                sy.append_block_md(&doc, &refs).await.with_context(|| {
                    format!(
                        "тело обновлено, но референсы не дописаны — повтори: task body {id} --attach <файлы>"
                    )
                })?;
            }
            println!("тело обновлено: {body_path} ({doc})");
            if !uploaded.is_empty() {
                let paths: Vec<&str> = uploaded.iter().map(|a| a.asset_path.as_str()).collect();
                println!("ассеты: {}", paths.join(", "));
            }
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
    let chain = syt::board_doc_chain(sy, &board).await?;
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
async fn task_clean(
    sy: &SiYuan,
    project: &str,
    ids: &str,
    yes: bool,
    live: Option<&str>,
) -> Result<()> {
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
    let chain = syt::board_doc_chain(sy, &board).await?;
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
                println!(
                    "  [{:<13}] «{}» (строка {}) — {}",
                    verdict.label(),
                    task.title,
                    task.item_id,
                    hint
                );
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
        let matched = planned
            .iter()
            .any(|(_, task, _)| guard::frag_matches(frag, task));
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
            refused.push(format!(
                "«{}» — Live: нужен --live <фрагмент> или исключи из --ids",
                task.title
            ));
        } else {
            refused.push(format!("«{}» — {}: не трогаю", task.title, verdict.label()));
        }
    }

    if !yes {
        println!(
            "dry-run: мутаций нет. Исполнение — повтор с --yes (журнал и пост-чек включатся сами)."
        );
        return Ok(());
    }
    if !refused.is_empty() {
        bail!(
            "батч отклонён ЦЕЛИКОМ, ноль мутаций:\n  - {}",
            refused.join("\n  - ")
        );
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
            extra,
            journal_path
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
            println!(
                "  {name}  {} [{}, {} строк(а)]",
                journal.created,
                journal.kind,
                journal.rows.len()
            );
        }
        println!("восстановление: task undo <имя> --project <проект журнала>");
        return Ok(());
    };
    let journal = Journal::load(sy, name).await?;
    if journal.project != project {
        bail!(
            "журнал принадлежит проекту «{}», а --project «{project}»",
            journal.project
        );
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
    let chain = syt::board_doc_chain(sy, &board).await?;
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
            sy.db_item_add(&board.av_id, &syt::path_safe(&row.title))
                .await?
        } else {
            sy.db_item_add_bound(&board.av_id, &body_doc).await?
        };
        restore_journal_cells(sy, &board, &journal.kind, &item, row).await?;
        println!(
            "  восстановлена: «{}» — строка {item} (была {}), тело {}",
            row.title,
            row.item_id,
            if body_doc.is_empty() {
                "—".to_string()
            } else {
                body_doc.clone()
            }
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
async fn restore_journal_body(
    sy: &SiYuan,
    board: &syt::SyBoard,
    row: &JournalRow,
) -> Result<String> {
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
            sy.create_doc_md(&board.notebook, &row.body_hpath, &clean_md)
                .await
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
            sy.db_item_set_status(&board.av_id, &sk, item, &row.status)
                .await?;
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
        sy.db_item_set_status(&board.av_id, &sk, item, &row.status)
            .await?;
    }
    if !row.branch.is_empty() {
        set_text_cells(sy, &board.av_id, item, &[("Branch", row.branch.as_str())]).await?;
    }
    if row.pr.is_empty() {
        return Ok(());
    }
    let keys = sy
        .mcp_tool("database", json!({ "action": "keys", "id": board.av_id }))
        .await?;
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
        None => bail!(
            "ключ PR не найден на доске — PR «{}» не восстановлен",
            row.pr
        ),
    }
}

// ——— person: персона владельца (SiYuan-док) ———

pub(crate) async fn person_run(command: PersonSub) -> Result<()> {
    let sy = SiYuan::from_env()?;
    let target = person_target();
    match command {
        PersonSub::Show => {
            let md = person::show(&sy, &target).await?;
            print!("{md}");
            Ok(())
        }
        PersonSub::Append { stdin, text } => {
            let md = collect_text(stdin, &text)?;
            if md.trim().is_empty() {
                bail!("append: нужен текст (позиционный или --stdin)");
            }
            let doc_id = person::append(&sy, &target, &md).await?;
            println!("дописано: {}:{} ({doc_id})", target.notebook, target.hpath);
            Ok(())
        }
        PersonSub::Replace { stdin, md } => {
            // attach_present=false: у персоны нет аттачей — ровно старая
            // семантика read_body_input (пустой контент бейлится ниже)
            let content = read_body_input(md.as_deref(), stdin, false)?;
            if content.trim().is_empty() {
                bail!(
                    "replace: пустой контент — персона не может быть пустой; правь/удаляй док в UI"
                );
            }
            let doc_id = person::replace(&sy, &target, &content).await?;
            println!(
                "персона перезаписана: {}:{} ({doc_id})",
                target.notebook, target.hpath
            );
            Ok(())
        }
    }
}

// ——— cron: строки доски «Кроны <proj>» ———

pub(crate) async fn cron_run(command: CronSub) -> Result<()> {
    let sy = SiYuan::from_env()?;
    match command {
        CronSub::List { project } => cron_list(&sy, &cron_project(project)).await,
        CronSub::Set {
            title,
            r#in,
            every,
            at,
            cron_expr,
            project,
            stdin,
        } => {
            let schedule = cron_schedule(&r#in, &every, &at, &cron_expr)?;
            let prompt = if stdin {
                read_stdin()?
            } else {
                "<промпт крона>".into()
            };
            let proj = cron_project(project);
            let board = syt::find_board_cron(&sy, &proj).await?;
            // колонки — до мутаций: строка без Schedule демону не видна,
            // а тело/строка, созданные впустую, — сироты (репро 04.11)
            let keys = syt::board_keys(&sy, &board.av_id).await?;
            let cron_keys = syt::require_cron_keys(&keys, &board.hpath)?;
            // дубль-тайтл: живая строка или тело-сирота (дедуп как в task create)
            let canon = syt::normalize_title(&title);
            if syt::read_rows(&sy, &board)
                .await?
                .iter()
                .any(|r| syt::normalize_title(&r.title) == canon)
            {
                anyhow::bail!("крон «{title}» уже есть (живая строка на {})", board.hpath);
            }
            if !syt::body_doc_by_title(&sy, &board, &title)
                .await?
                .is_empty()
            {
                anyhow::bail!("тело крона «{title}» уже есть");
            }
            // тело → bound-строка (клик по карточке открывает промпт) →
            // клетки → verify вставки в render (репро 04.11 08:27: success
            // item_add не гарантирует строку), паттерн task create
            let body_h = board.body_hpath(&title);
            let body = sy
                .create_doc_md(&board.notebook, &body_h, &cron_body_md(&title, &prompt))
                .await?;
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
    sy.db_item_set_status(&board.av_id, &keys.status, &item, "active")
        .await?;
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

async fn cron_set_status(
    sy: &SiYuan,
    id: &str,
    status: &str,
    verb: &str,
    project: &str,
) -> Result<()> {
    let board = syt::find_board_cron(sy, project).await?;
    let task = syt::resolve(sy, &board, id).await?;
    let key = syt::status_key(sy, &board.av_id).await?;
    sy.db_item_set_status(&board.av_id, &key, &task.item_id, status)
        .await?;
    println!("крон {verb}: {}", task.title);
    Ok(())
}

/// Текстовые клетки по именам колонок. Отсутствующая колонка — ошибка:
/// тихий скип оставлял строки без Schedule — демон не видел расписание
/// (репро 04.11, задача 20261104220156).
async fn set_text_cells(sy: &SiYuan, av: &str, item: &str, cells: &[(&str, &str)]) -> Result<()> {
    let keys = syt::board_keys(sy, av).await?;
    for (name, value) in cells {
        let Some(key) = keys
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, id)| id.as_str())
        else {
            anyhow::bail!("колонка «{name}» не найдена на доске {av} — ячейка не записана");
        };
        write_text_cell(sy, av, key, item, value).await?;
    }
    Ok(())
}

fn cron_schedule(
    in_: &Option<String>,
    every: &Option<String>,
    at: &Option<String>,
    expr: &Option<String>,
) -> Result<String> {
    let opts = [
        in_.as_deref(),
        every.as_deref(),
        at.as_deref(),
        expr.as_deref(),
    ];
    let n = opts.iter().filter(|o| o.is_some()).count();
    if n != 1 {
        anyhow::bail!("нужен ровно один из --in / --every / --at / --cron (получено {n})");
    }
    Ok(opts.into_iter().flatten().next().unwrap().to_string())
}

// ——— docs/search/blob/projects ———

/// Голова stdout search — контракт фабричных агентов (парсят вывод);
/// единственная точка формата, правка = красный golden-тест.
fn search_head(query: &str) -> String {
    format!("Searching for: \"{query}\"")
}

pub(crate) async fn search_run(
    query: &str,
    limit: usize,
    threshold: f32,
    sofa: usize,
) -> Result<()> {
    let sy = SiYuan::from_env()?;
    println!("{}\n", search_head(query));
    // Полнотекстовая секция — формат и состав байт-в-байт прежние:
    // вывод парсят фабричные агенты (контракт регрессии задачи 20261104072044).
    let rows = sy.search(query, limit).await?;
    for (i, r) in rows.iter().enumerate() {
        let hpath = r.get("hpath").and_then(|h| h.as_str()).unwrap_or("");
        let content = r.get("content").and_then(|c| c.as_str()).unwrap_or("");
        println!("{}. {}\n    {}", i + 1, hpath, content.replace('\n', " "));
    }
    if rows.is_empty() {
        println!("ничего не найдено");
    }
    if sofa > 0 {
        print_sofa(query, sofa).await;
    }
    // Семантическая секция — always-on: отказы маршрута становятся
    // статусом внутри секции, команда не падает (exit 0).
    let cfg = crate::semantic::SemanticConfig::from_env();
    let section = crate::semantic::search_half(&sy, &cfg, query, limit, threshold).await;
    println!();
    for line in section {
        println!("{line}");
    }
    Ok(())
}

/// SOFA-блок полнотекстовой секции (opt-in): софт-деградация без кред —
/// предупреждение в stderr, локальный поиск не страдает.
async fn print_sofa(query: &str, limit: usize) {
    println!("\nSOFA (публичный):");
    match crate::sofa::search(query, limit).await {
        Ok(hits) if hits.is_empty() => println!("ничего не найдено"),
        Ok(hits) => {
            for (i, hit) in hits.iter().enumerate() {
                println!("{}. {} [{}]", i + 1, hit.title, hit.content_type);
                println!("    {}", hit.url);
            }
        }
        Err(e) => eprintln!("SOFA недоступен (продолжаю локально): {e:#}"),
    }
}

/// Обслуживание semantic-индекса: явный пересбор (громкие ошибки —
/// maintenance-команда) и статус.
pub(crate) async fn semantic_run(command: SemanticSub) -> Result<()> {
    let sy = SiYuan::from_env()?;
    let cfg = crate::semantic::SemanticConfig::from_env();
    match command {
        SemanticSub::Rebuild { resume } => {
            if resume {
                eprintln!(
                    "Продолжение semantic-индекса от watermark: {} через {}",
                    cfg.embed_model, cfg.embed_url
                );
            } else {
                eprintln!(
                    "Пересбор semantic-индекса: {} через {} (первичный билд ~55–70 мин)",
                    cfg.embed_model, cfg.embed_url
                );
            }
            let batches = std::sync::Mutex::new(0usize);
            let on_batch = std::sync::Arc::new(move |embedded: usize| {
                let mut count = batches.lock().expect("progress lock");
                *count += 1;
                if (*count).is_multiple_of(10) {
                    eprintln!("  чанков записано: {embedded}");
                }
            });
            let report = crate::semantic::rebuild(&sy, &cfg, Some(on_batch), resume).await?;
            println!(
                "Готово: {} чанков за {:.0} мин (пропущено коротких: {})",
                report.embedded,
                report.elapsed.as_secs_f64() / 60.0,
                report.skipped_short,
            );
        }
        SemanticSub::Status => {
            for line in crate::semantic::status(&sy, &cfg).await? {
                println!("{line}");
            }
        }
        SemanticSub::Sync => {
            for line in crate::semantic::sync_once(&sy, &cfg).await? {
                println!("{line}");
            }
        }
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
                if entry
                    .get("isDir")
                    .and_then(|d| d.as_bool())
                    .unwrap_or(false)
                {
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
            sy.post_probe("/api/query/sql", json!({ "stmt": "select 1" }))
                .await?;
            println!("alive: sql ok");
        }
        KernelSub::Sql { stmt } => {
            let envelope = sy
                .post_probe("/api/query/sql", json!({ "stmt": stmt }))
                .await?;
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
            let envelope = sy
                .post_probe("/api/file/readDir", json!({ "path": path }))
                .await?;
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
                eprintln!(
                    "(export пуст: 0 символов — док пуст, деградация индекса или неверный id)"
                );
            }
            print!("{content}");
        }
        KernelSub::Render {
            av_id,
            block_id,
            page,
        } => {
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
/// Отказ гарда секции — Err до overwrite: экспорт read-only, ядро не тронуто.
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
            let (md, edit) = replace_md_section(&existing, section, content)?;
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
/// Запрос нормализован от собственного ATX-префикса: «## Решение» матчит
/// секцию «Решение» (иначе want нёс бы хэши, матч не сходился, а append
/// писал мусор «## ## Решение»). Гард уровня: h2-матча нет, но тёзка «X» есть
/// на уровне 1 или ≥ 3 — Err с уровнем и байт-точной строкой (первой по
/// порядку): иначе контент молча аппендится новой ##-секцией в конец — дубль
/// (ожог ###-правок). Тёзка в фенсе не заголовок — гард молчит; при живом
/// h2-матче тёзка игнорируется. Секция не найдена вовсе — append в конец
/// с нормализованным заголовком; пустой ввод очищает секцию (контракт);
/// пустая строка перед следующим заголовком восстанавливается.
fn replace_md_section(md: &str, section: &str, content: &str) -> Result<(String, SectionEdit)> {
    let want = md_heading::normalize_heading_query(section);
    let mut out = String::new();
    let mut fenced = false;
    let mut dropping = false; // контент секции/дубля до границы не копируется
    let mut replaced = false;
    let mut duplicates_removed = 0usize;
    // Тёзка на чужом уровне: собирается в тот же проход, гард стреляет
    // только если h2-матча не было; первая по порядку — детерминизм ошибки.
    let mut twin: Option<(usize, &str)> = None;
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
                if text == want && twin.is_none() {
                    twin = Some((level, line));
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
        if let Some((level, twin_line)) = twin {
            bail!(
                "секция «{section}» на уровне ## не найдена, но есть тёзка на уровне h{level}: «{twin_line}»; \
                 --replace-section матчит только ## — правь через содержащую ##-секцию или полный task body --replace"
            );
        }
        let heading = format!("## {want}");
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&heading);
        out.push('\n');
        if !content.trim().is_empty() {
            out.push_str(content.trim());
            out.push('\n');
        }
        return Ok((out, SectionEdit::Appended));
    }
    Ok((out, SectionEdit::Replaced { duplicates_removed }))
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

fn read_body_input(md_file: Option<&str>, stdin: bool, attach_present: bool) -> Result<String> {
    if let Some(f) = md_file {
        return Ok(std::fs::read_to_string(f)?);
    }
    if stdin {
        return read_stdin();
    }
    if attach_present {
        // только аттачи, контента нет — валидный вызов
        return Ok(String::new());
    }
    anyhow::bail!("нужно --stdin или --md <файл>")
}

/// Файлы аттачей: существуют и это файлы — до любых сетевых вызовов.
fn validate_attach_files(files: &[std::path::PathBuf]) -> Result<&[std::path::PathBuf]> {
    for file in files {
        let meta = std::fs::metadata(file)
            .with_context(|| format!("файл {}: недоступен", file.display()))?;
        anyhow::ensure!(meta.is_file(), "{} — не файл (директория?)", file.display());
    }
    Ok(files)
}

/// Расширения, которые SiYuan рендерит картинками; прочие аттачи — ссылки.
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "svg", "webp", "apng", "avif", "bmp", "ico",
];

/// Сегменты пути → байты вне RFC3986-unreserved → %XX (upper hex); `/`
/// остаётся литеральным разделителем. Симметрично экспорту ядра (пробелы
/// %20, кириллица %D0%B8… — фикстура collect_asset_refs) и его читателю:
/// префикс `](assets/` у читателя литеральный, `%2F` его разорвал бы
/// (поймано round-trip тестом), сырой пробел/`)` в имени режет ссылку.
fn percent_encode_asset_path(path: &str) -> String {
    path.split('/')
        .map(percent_encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn percent_encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn is_image_name(name: &str) -> bool {
    name.rsplit('.')
        .next()
        .map(|ext| ext.to_lowercase())
        .is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.as_str()))
}

/// Markdown-референс ассета в теле: картинка `![имя](assets/…)` или
/// ссылка `[имя](assets/…)`. Alt-текст без `[`/`]` — не рвать markdown.
fn asset_ref_md(asset: &crate::siyuan::UploadedAsset) -> String {
    let alt = asset.original_name.replace(['[', ']'], "");
    let encoded = percent_encode_asset_path(&asset.asset_path);
    let prefix = if is_image_name(&asset.original_name) {
        "!"
    } else {
        ""
    };
    format!("{prefix}[{alt}]({encoded})")
}

/// Блок референсов для append-а в тело: ведущая пустая строка отделяет
/// от контента (шаблона/тега типа), по строке на ассет.
fn attach_refs_md(assets: &[crate::siyuan::UploadedAsset]) -> String {
    let mut out = String::from("\n");
    for asset in assets {
        out.push_str(&asset_ref_md(asset));
        out.push('\n');
    }
    out
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

/// Пред-гвард позиционного текста коммента: выполняется в task_run ДО bail по
/// пустому --project, чтобы проглоченный trailing_var_arg'ом флаг
/// (`task comment <id> "текст" --project X`) не маскировался под «--project не
/// передан». stdin — no-op: его содержимое недоступно до чтения, читает его
/// ровно одна ветка (двойное чтение дало бы EOF → ложный «пустой текст»).
/// Позиционная валидация чистая и идемпотентна — повтор в Comment-ветке безвреден.
fn pre_guard_positional_comment(stdin: bool, text: &[String]) -> Result<()> {
    if stdin {
        return Ok(());
    }
    validate_comment_text(false, &text.join(" "))
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
    key.split('/')
        .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
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
    use super::search_head;
    use super::task_run;
    use super::validate_comment_text;
    use super::SectionEdit;
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
    const BOUND_DOC: &str = "20261105120000-boundd1";
    const ORPHAN_DOC: &str = "20261105090000-orphan1";

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
    ) -> (
        MockServer,
        Arc<AtomicUsize>,
        Arc<Mutex<Vec<String>>>,
        Arc<AtomicUsize>,
    ) {
        mock_siyuan_with_export(title_doc_ids, with_binding, String::new()).await
    }

    /// Расширенный мок: с экспортом дока (обёртка frontmatter + имя) для
    /// rewrite-пути (replace/--replace-section). Четвёртый элемент — счётчик
    /// deleteBlock: overwrite-путь удаляет блоки «штормом», no-write-тесты
    /// ассертят его пустоту против именно опасного вызова.
    async fn mock_siyuan_with_export(
        title_doc_ids: Vec<String>,
        with_binding: bool,
        export_content: String,
    ) -> (
        MockServer,
        Arc<AtomicUsize>,
        Arc<Mutex<Vec<String>>>,
        Arc<AtomicUsize>,
    ) {
        let server = MockServer::start().await;
        let create_calls = Arc::new(AtomicUsize::new(0));
        let append_bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let delete_calls = Arc::new(AtomicUsize::new(0));

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
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"code":-1,"msg":"createDocWithMd must not be called"}"#)
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

        let deletes = delete_calls.clone();
        Mock::given(method("POST"))
            .and(path("/api/block/deleteBlock"))
            .respond_with(move |_req: &wiremock::Request| {
                deletes.fetch_add(1, Ordering::SeqCst);
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
                ResponseTemplate::new(200)
                    .set_body_string(serde_json::json!({ "code": 0, "data": data }).to_string())
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

        (server, create_calls, append_bodies, delete_calls)
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
                attach: Vec::new(),
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
        let (server, create_calls, append_bodies, _deletes) =
            mock_siyuan(vec![ORPHAN_DOC.to_string(), BOUND_DOC.to_string()], true).await;
        write_body_via_cli(&server).await.unwrap();

        assert_eq!(
            create_calls.load(Ordering::SeqCst),
            0,
            "createDocWithMd не должен вызываться"
        );
        {
            let appends = append_bodies.lock().unwrap();
            assert_eq!(appends.len(), 1, "один appendBlock в привязанный док");
            assert!(
                appends[0].contains("маркер-контент"),
                "контент в appendBlock"
            );
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
        let export =
            "---\ntitle: таск\n---\n\n# таск\n\n## Проверка\nстарое тело секции\n".to_string();
        let (server, create_calls, append_bodies, _deletes) =
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
                attach: Vec::new(),
            },
        )
        .await
        .unwrap();

        assert_eq!(create_calls.load(Ordering::SeqCst), 0);
        {
            let appends = append_bodies.lock().unwrap();
            assert_eq!(appends.len(), 1, "один appendBlock пересборки");
            let payload = &appends[0];
            assert!(
                payload.contains("новое содержимое секции"),
                "новая секция в appendBlock"
            );
            assert!(
                !payload.contains("# таск"),
                "имя дока не реимпортируется блоком"
            );
            assert!(
                !payload.contains("title: таск"),
                "frontmatter не реимпортируется блоком"
            );
            assert!(
                !payload.contains("старое тело секции"),
                "старая секция заменена"
            );
        }
    }

    /// Гард уровня на мок-ядре: тёзка «Подсекция» только на ### → task_run
    /// возвращает Err с уровнем и строкой, записи в ядро нет — ни appendBlock
    /// пересборки, ни deleteBlock-шторма overwrite_doc_md (тот, что валил ядро).
    #[tokio::test]
    #[serial_test::serial]
    async fn replace_section_twin_other_level_refuses_without_kernel_write() {
        let export =
            "---\ntitle: таск\n---\n\n# таск\n\n## Родитель\nввод\n### Подсекция\nдетали\n"
                .to_string();
        let (server, create_calls, append_bodies, delete_calls) =
            mock_siyuan_with_export(vec![BOUND_DOC.to_string()], true, export).await;
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
        let md = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(md.path(), "новое содержимое\n").unwrap();
        let err = task_run(
            "uwuwu",
            TaskSub::Body {
                id: "таск".into(),
                stdin: false,
                md: Some(md.path().to_string_lossy().into_owned()),
                replace: false,
                replace_section: Some("Подсекция".into()),
                attach: vec![],
            },
        )
        .await
        .unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("h3") && msg.contains("### Подсекция"),
            "суть отказа: {msg}"
        );
        assert_eq!(create_calls.load(Ordering::SeqCst), 0, "доки не создаются");
        {
            let appends = append_bodies.lock().unwrap();
            assert!(
                appends.is_empty(),
                "appendBlock не вызывался: {:?}",
                *appends
            );
        }
        assert_eq!(
            delete_calls.load(Ordering::SeqCst),
            0,
            "deleteBlock-шторма нет — ядро не тронуто"
        );
    }

    /// Detached-строка (привязки нет) + ≥2 доков с тем же заголовком →
    /// явная ошибка со списком docId, не молчаливая запись в «первый».
    #[tokio::test]
    #[serial_test::serial]
    async fn body_errors_listing_duplicates_for_detached_row() {
        let (server, _create, _append, _deletes) =
            mock_siyuan(vec![ORPHAN_DOC.to_string(), "doc-b".to_string()], false).await;
        let err = write_body_via_cli(&server).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("несколько доков"), "суть ошибки: {msg}");
        assert!(
            msg.contains(ORPHAN_DOC) && msg.contains("doc-b"),
            "список docId в ошибке: {msg}"
        );
    }

    #[test]
    fn replace_md_section_replaces_body_keeps_heading() {
        let md = "## А\nстарое\n\n## Б\nвторое\n";
        let (out, edit) = replace_md_section(md, "А", "новое содержимое\n").unwrap();
        assert!(out.contains("## А\nновое содержимое"));
        assert!(out.contains("## Б\nвторое"));
        assert_eq!(
            edit,
            SectionEdit::Replaced {
                duplicates_removed: 0
            }
        );
    }

    #[test]
    fn replace_md_section_appends_missing_section() {
        let md = "## А\nпервое\n";
        let (out, edit) = replace_md_section(md, "Новая", "контент").unwrap();
        assert!(out.contains("## Новая\nконтент"));
        assert!(out.contains("## А\nпервое"));
        assert_eq!(edit, SectionEdit::Appended);
    }

    #[test]
    fn replaces_first_and_removes_duplicate_sections() {
        let md = "# Таск\n\n## Причина\nпервое вхождение\n\n## Причина\nвторое вхождение\n\n## Фикс\nправка\n";
        let (out, edit) = replace_md_section(md, "Причина", "новый механизм\n").unwrap();
        assert_eq!(out.matches("## Причина").count(), 1, "секция остаётся одна");
        assert!(
            !out.contains("первое вхождение"),
            "старый контент первого удалён"
        );
        assert!(!out.contains("второе вхождение"), "контент дубля удалён");
        assert!(
            out.contains("## Фикс\nправка"),
            "соседняя секция не тронута"
        );
        assert_eq!(
            edit,
            SectionEdit::Replaced {
                duplicates_removed: 1
            }
        );
    }

    #[test]
    fn ends_section_at_higher_level_heading() {
        let md = "## А\nстарое\n# Заголовок H1\nхвост тайтла\n\n## Б\nвторое\n";
        let (out, _) = replace_md_section(md, "А", "новое\n").unwrap();
        assert!(out.contains("# Заголовок H1"), "h1 переживает replace");
        assert!(
            out.contains("хвост тайтла"),
            "контент после h1 переживает replace"
        );
        assert!(out.contains("## Б\nвторое"));
    }

    #[test]
    fn h3_stays_inside_section() {
        let md =
            "## А\nввод\n### Подсекция\nдетали подсекции\n\n## Б\nвторое\n### Б-под\nб-детали\n";
        let (out, _) = replace_md_section(md, "А", "новое\n").unwrap();
        assert!(
            !out.contains("детали подсекции"),
            "h3 не рвёт диапазон: контент под h3 заменяется вместе с секцией"
        );
        assert!(
            out.contains("### Б-под\nб-детали"),
            "h3 чужой секции не тронут"
        );
        assert!(out.contains("## Б\nвторое"));
    }

    #[test]
    fn normalizes_heading_and_argument() {
        let md = "##  Постановка\nстарое\n";
        let (out, edit) = replace_md_section(md, " Постановка ", "новое\n").unwrap();
        assert!(out.contains("новое"), "матч по нормализованному тексту");
        assert_eq!(out.matches("##").count(), 1, "без append-дубля секции");
        assert_eq!(
            edit,
            SectionEdit::Replaced {
                duplicates_removed: 0
            }
        );
    }

    #[test]
    fn heading_preserved_byte_for_byte() {
        let md = "##  Причина ^20261104120000-ab12cd\nстарое\n\n## Б\nвторое\n";
        let (out, _) = replace_md_section(md, "Причина", "новое\n").unwrap();
        assert!(
            out.contains("##  Причина ^20261104120000-ab12cd\nновое"),
            "заматченный заголовок пишется исходной строкой (пробелы и ^id живы)"
        );
        assert_eq!(out.matches("##").count(), 2, "без append-дубля");
    }

    #[test]
    fn matches_heading_with_block_id_suffix() {
        let md = "## Причина ^20261104120000-ab12cd\nстарое\n\n## Б\nвторое\n";
        let (out, edit) = replace_md_section(md, "Причина", "новое\n").unwrap();
        assert!(out.contains("новое"), "суффикс ^id не мешает матчу");
        assert_eq!(
            edit,
            SectionEdit::Replaced {
                duplicates_removed: 0
            }
        );
    }

    #[test]
    fn fenced_h2_does_not_bound_section() {
        let md = "## А\nстарое\n```md\n## Фейк внутри фенса\n```\nхвост секции\n\n## Б\nвторое\n";
        let (out, _) = replace_md_section(md, "А", "новое\n").unwrap();
        assert!(
            !out.contains("Фейк"),
            "h2 внутри фенса — контент секции, заменяется"
        );
        assert!(!out.contains("хвост секции"), "секция длится сквозь фенс");
        assert!(out.contains("## Б\nвторое"));
    }

    #[test]
    fn hash_line_inside_fence_survives() {
        let md = "## Сценарий (BDD)\nстарое\n```gherkin\nСценарий: x\n  Дано y\n  Тогда z  # факт\n```\n\n## Б\nвторое\n";
        let (out, _) = replace_md_section(md, "Сценарий (BDD)", "новое\n").unwrap();
        assert!(
            out.contains("## Б\nвторое"),
            "граница секции — настоящий заголовок Б"
        );
        assert_eq!(out.matches("## Б").count(), 1, "секция Б не задвоена");
        assert!(
            !out.contains("# факт"),
            "старый контент секции заменён целиком"
        );
    }

    #[test]
    fn empty_content_clears_section() {
        let md = "## А\nстарое\n\n## Б\nвторое\n";
        let (out, _) = replace_md_section(md, "А", "").unwrap();
        assert!(out.contains("## А"), "заголовок сохранён");
        assert!(!out.contains("старое"), "контент очищен пустым вводом");
        assert!(out.contains("## Б\nвторое"));
    }

    #[test]
    fn blank_line_restored_before_next_heading() {
        let md = "## А\nстарое\n\n## Б\nвторое\n";
        let (out, _) = replace_md_section(md, "А", "новое\n").unwrap();
        assert!(
            out.contains("новое\n\n## Б"),
            "пустая строка между секциями восстановлена"
        );
    }

    // ——— гард уровня: h2-матча нет, тёзка на уровне 1 или ≥3 → отказ, записи нет ———

    #[test]
    fn h3_twin_refuses_with_level_and_byte_line() {
        let md = "## Родитель\nввод\n### Подсекция\nдетали подсекции\n";
        let err = replace_md_section(md, "Подсекция", "новое\n").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("h3"), "уровень тёзки в ошибке: {msg}");
        assert!(
            msg.contains("### Подсекция"),
            "байт-точная строка заголовка: {msg}"
        );
        assert!(msg.contains("--replace"), "рецепт обхода в ошибке: {msg}");
    }

    #[test]
    fn h1_twin_refuses_replacement() {
        let md = "# Подсекция\nтело тайтла\n";
        let err = replace_md_section(md, "Подсекция", "новое\n").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("h1"),
            "h1-тёзка гардится тем же правилом: {msg}"
        );
    }

    #[test]
    fn twin_detection_is_normalized_double_space_and_block_id() {
        // регулярный прод-экспорт несёт двойные пробелы и ^block-id: детект
        // тёзки по сырой строке промолчал бы ровно на этом случае
        let md = "## Родитель\nввод\n###  Подсекция ^20261105230000-ab12cd\nдетали\n";
        let err = replace_md_section(md, "Подсекция", "новое\n").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("###  Подсекция ^20261105230000-ab12cd"),
            "байт-точная строка с пробелами и ^id: {msg}"
        );
    }

    #[test]
    fn twin_inside_fence_appends_instead_of_guard() {
        let md = "## Родитель\n```md\n### Подсекция\n```\nхвост\n";
        let (out, edit) = replace_md_section(md, "Подсекция", "новое\n").unwrap();
        assert!(
            out.contains("## Подсекция\nновое"),
            "тёзка в фенсе — не заголовок, гард молчит: легитимный append"
        );
        assert_eq!(edit, SectionEdit::Appended);
    }

    #[test]
    fn h2_match_wins_over_h3_twin_without_guard() {
        let md = "## Подсекция\nстарое\n\n## Другое\nввод\n### Подсекция\nдетали\n";
        let (out, edit) = replace_md_section(md, "Подсекция", "новое\n").unwrap();
        assert!(
            out.contains("## Подсекция\nновое"),
            "h2-матч заменён на месте"
        );
        assert!(
            out.contains("### Подсекция\nдетали"),
            "###-тёзка вне секции не тронута"
        );
        assert_eq!(
            edit,
            SectionEdit::Replaced {
                duplicates_removed: 0
            },
            "гард не стреляет при живом h2"
        );
    }

    #[test]
    fn first_twin_in_document_order_named_in_error() {
        let md = "### Подсекция\nверхняя\n\n# Подсекция\nнижняя\n";
        let err = replace_md_section(md, "Подсекция", "новое\n").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("h3"),
            "первая тёзка по порядку — детерминизм: {msg}"
        );
        assert!(msg.contains("### Подсекция"), "строка первой тёзки: {msg}");
        assert!(!msg.contains("h1"), "вторая тёзка не упоминается: {msg}");
    }

    // ——— нормализация запроса: ATX-префикс в --replace-section срезается ———

    #[test]
    fn prefixed_h3_query_refuses_instead_of_garbage_append() {
        // факт 06.11 (задача 20261104072048): запрос «### Замечания…» при живой
        // h3-секции → раньше not-found append «## ### Замечания…» в хвост тела
        let md = "## Финальный отчёт\nввод\n### Замечания и неочевидные решения\nдетали\n\n## Комменты\n- строка\n";
        let err =
            replace_md_section(md, "### Замечания и неочевидные решения", "новое\n").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("h3"),
            "запрос с префиксом матчит тёзку после среза хэшей: {msg}"
        );
        assert!(
            msg.contains("### Замечания и неочевидные решения"),
            "байт-точная строка: {msg}"
        );
    }

    #[test]
    fn prefixed_h2_query_matches_live_section() {
        let md = "## X\nстарое\n\n## Б\nвторое\n";
        let (out, edit) = replace_md_section(md, "## X", "новое\n").unwrap();
        assert!(
            out.contains("## X\nновое"),
            "запрос «## X» матчит живую секцию «X»"
        );
        assert!(out.contains("## Б\nвторое"));
        assert_eq!(out.matches("## X").count(), 1, "без дубля секции");
        assert_eq!(
            edit,
            SectionEdit::Replaced {
                duplicates_removed: 0
            }
        );
    }

    #[test]
    fn prefixed_query_appends_clean_heading() {
        let md = "## А\nтело\n";
        let (out, edit) = replace_md_section(md, "### Новая", "контент").unwrap();
        assert!(
            out.contains("## Новая\nконтент"),
            "append пишет чистый заголовок без хэшей запроса"
        );
        assert!(
            !out.contains("## ###"),
            "двойного префикса в теле нет: {out}"
        );
        assert_eq!(edit, SectionEdit::Appended);
    }

    #[test]
    fn collect_threads_prints_normalized_section() {
        let md = "## Постановка\nтело\n\n##  Комменты\n- **AGENTS**: вопрос\n";
        let out = collect_threads(md);
        assert!(
            out.contains("##  Комменты"),
            "заголовок с двойным пробелом матчится"
        );
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
        assert!(
            out.contains("- второй"),
            "дубли секций печатаются обе — read-путь ничего не роняет"
        );
    }

    #[test]
    fn collect_threads_prints_hash_tag_inside_section() {
        let md = "## Комменты\n- коммент\n#bug\n- коммент после тега\n";
        let out = collect_threads(md);
        assert!(
            out.contains("#bug"),
            "#bug без пробела — не заголовок, секцию не закрывает"
        );
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
        assert!(
            err.to_string().contains("task get/thread"),
            "подсказка чтения: {err}"
        );
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
        validate_comment_text(false, "- пункт один\n- ещё пункт")
            .expect("markdown-буллет — контент, не флаг");
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
        validate_comment_text(true, "--не-флаг-а-текст")
            .expect("stdin — явное намерение, не гвардим");
        validate_comment_text(true, "list из пайпа — тоже контент")
            .expect("stdin: подкоманды не гвардятся");
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
        assert!(
            msg.contains("похож на флаг"),
            "реальная причина в ошибке: {msg}"
        );
        assert!(
            !msg.contains("обязателен"),
            "маскировка «флаг не передан» недопустима: {msg}"
        );
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
        assert!(
            !msg.contains("обязателен"),
            "приоритет ошибок сменился обратно: {msg}"
        );
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

    // ——— attach: кодер референсов (симметрия с читателем collect_asset_refs) ———

    use super::asset_ref_md;
    use super::attach_refs_md;
    use super::percent_encode_asset_path;
    use super::validate_attach_files;
    use crate::siyuan::UploadedAsset;

    fn asset(name: &str, path: &str) -> UploadedAsset {
        UploadedAsset {
            original_name: name.into(),
            asset_path: path.into(),
        }
    }

    #[test]
    fn percent_encoding_round_trips_through_reader_on_space_and_cyrillic() {
        for name in [
            "скрин 2.png",
            "имя я.png",
            "снимок экрана (1).png",
            "отчёт.pdf",
        ] {
            let path = format!("assets/{name}");
            let md = asset_ref_md(&asset(name, &path));
            let decoded = collect_asset_refs(&md);
            assert_eq!(decoded, vec![name.to_string()], "round-trip {name}: {md}");
        }
    }

    #[test]
    fn percent_encoding_covers_unreserved_only() {
        assert_eq!(
            percent_encode_asset_path("assets/a b.png"),
            "assets/a%20b.png"
        );
        assert_eq!(
            percent_encode_asset_path("assets/а.png"),
            "assets/%D0%B0.png"
        );
        assert_eq!(
            percent_encode_asset_path("assets/ok-_.~1.txt"),
            "assets/ok-_.~1.txt"
        );
        assert_eq!(
            percent_encode_asset_path("assets/x(1).png"),
            "assets/x%281%29.png"
        );
    }

    #[test]
    fn asset_ref_md_renders_image_for_image_extension_and_link_otherwise() {
        let img = asset_ref_md(&asset("скрин.png", "assets/скрин.png"));
        assert!(img.starts_with("![скрин.png]("), "картинка: {img}");
        assert!(img.contains("%D1%81"), "кириллица пути закодирована: {img}");
        let file = asset_ref_md(&asset("отчёт.pdf", "assets/отчёт.pdf"));
        assert!(
            file.starts_with("[отчёт.pdf](") && !file.starts_with("!["),
            "ссылка: {file}"
        );
    }

    #[test]
    fn asset_ref_md_alt_text_without_brackets_keeps_markdown_intact() {
        let md = asset_ref_md(&asset("weird]name.png", "assets/weird.png"));
        let decoded = collect_asset_refs(&md);
        assert_eq!(
            decoded,
            vec!["weird.png".to_string()],
            "ссылка не разорвана: {md}"
        );
    }

    #[test]
    fn attach_refs_md_starts_with_blank_line_one_ref_per_line() {
        let md = attach_refs_md(&[
            asset("a.png", "assets/a.png"),
            asset("b.pdf", "assets/b.pdf"),
        ]);
        assert!(
            md.starts_with("\n"),
            "пустая строка-разделитель от контента: {md:?}"
        );
        let lines: Vec<&str> = md.trim().lines().collect();
        assert_eq!(lines.len(), 2, "по строке на ассет: {md:?}");
        assert!(lines[0].starts_with("![") && lines[1].starts_with("["));
    }

    #[test]
    fn validate_attach_files_bails_on_missing_and_directory_before_network() {
        let dir = tempfile::tempdir().unwrap();
        let ok = dir.path().join("ok.png");
        std::fs::write(&ok, b"x").unwrap();
        let missing = dir.path().join("нет.png");
        let err =
            validate_attach_files(&[ok.clone(), missing]).expect_err("несуществующий файл — отказ");
        assert!(err.to_string().contains("нет.png"), "суть: {err}");
        let err =
            validate_attach_files(&[dir.path().to_path_buf()]).expect_err("директория — отказ");
        assert!(err.to_string().contains("не файл"), "суть: {err}");
        assert_eq!(validate_attach_files(&[ok]).unwrap().len(), 1);
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
                    ResponseTemplate::new(404)
                        .set_body_string(r#"{"code":404,"msg":"path does not exist"}"#)
                }
            })
            .mount(&server)
            .await;
        let sy = SiYuan::for_tests(server.uri());

        const ROW: &str = "20261105090000-assette1";
        let md = "![и](assets/assets-self-test/img.png) ![нет](assets/assets-self-test/gone.png)\n";
        let saved = save_body_assets(&sy, md, ROW).await;

        assert_eq!(
            saved.len(),
            1,
            "доступный аттач сохранён, Missing — тихо пропущен: {saved:?}"
        );
        assert!(
            saved[0].ends_with("/img.png"),
            "оригинальное имя файла: {saved:?}"
        );
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
                    ResponseTemplate::new(404)
                        .set_body_string(r#"{"code":404,"msg":"path does not exist"}"#)
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

    // ——— edit_title: renameDoc + verify Title-клетки (stateful render) ———

    use crate::siyuan_task::{edit_title_with_pacing, SyTask, TitleSyncPacing};
    use serde_json::{json, Value};

    const CHAIN_ROOT: &str = "uwuwu-root";
    const OTHER_ROW: &str = "item-other";
    const OTHER_TITLE: &str = "чужой";
    const ORPHAN_TITLE: &str = "сирота";

    /// Мок под edit_title: id-цепочка доски (board_doc_chain), stateful render
    /// (renameDoc-маунт кладёт новый тайтл в общее состояние — так живёт
    /// verify-поллинг), renameDoc-маунт с записью запроса. `stateful_rename`
    /// = false моделирует «зависшую» клетку: rename проходит, render-состояние
    /// не меняется.
    async fn mock_siyuan_for_title(
        stateful_rename: bool,
    ) -> (
        MockServer,
        Arc<Mutex<Option<String>>>,
        Arc<Mutex<Vec<String>>>,
    ) {
        let server = MockServer::start().await;
        let renamed_to: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let renames: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        Mock::given(method("POST"))
            .and(path("/api/filetree/getIDsByHPath"))
            .respond_with(|req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let hpath = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
                let data = match hpath {
                    "/uwuwu" => serde_json::json!([CHAIN_ROOT]),
                    "/uwuwu/Задачи uwuwu" => serde_json::json!([BOARD_DOC]),
                    // «новый тайтл» отдаёт СОБСТВЕННЫЙ док: happy-путь проверяет
                    // исключение своего docId из orphan-чека (retry после лага)
                    "/uwuwu/Задачи uwuwu/новый тайтл" => {
                        serde_json::json!([BOUND_DOC])
                    }
                    "/uwuwu/Задачи uwuwu/сирота" => serde_json::json!([ORPHAN_DOC]),
                    _ => serde_json::json!([]),
                };
                ResponseTemplate::new(200)
                    .set_body_string(serde_json::json!({ "code": 0, "data": data }).to_string())
            })
            .mount(&server)
            .await;

        let renamed_for_mount = renamed_to.clone();
        let renames_for_mount = renames.clone();
        Mock::given(method("POST"))
            .and(path("/api/filetree/renameDoc"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                renames_for_mount.lock().unwrap().push(
                    body.get("path")
                        .and_then(|p| p.as_str())
                        .unwrap_or("")
                        .to_string(),
                );
                if stateful_rename {
                    *renamed_for_mount.lock().unwrap() = Some(
                        body.get("title")
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string(),
                    );
                }
                ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":null}"#)
            })
            .mount(&server)
            .await;

        let renamed_for_render = renamed_to.clone();
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(move |req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("\"initialize\"") {
                    return ResponseTemplate::new(200)
                        .set_body_string(
                            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"3"}}}"#,
                        )
                        .append_header("mcp-session-id", "sess-1");
                }
                if body.contains("notifications/initialized") {
                    return ResponseTemplate::new(200);
                }
                if body.contains("tools/call") {
                    let current = renamed_for_render.lock().unwrap().clone();
                    let own_title = current.unwrap_or_else(|| "таск".into());
                    let view = serde_json::json!({
                        "columns": [{ "id": TITLE_KEY, "name": "Primary Key" }],
                        "rows": [
                            { "id": ROW_ID, "cells": [{ "value": { "keyID": TITLE_KEY,
                                "type": "block",
                                "block": { "content": own_title, "id": BOUND_DOC, "refSubtype": "d" } } }] },
                            { "id": OTHER_ROW, "cells": [{ "value": { "keyID": TITLE_KEY,
                                "type": "block",
                                "block": { "content": OTHER_TITLE, "id": ORPHAN_DOC, "refSubtype": "d" } } }] },
                        ],
                    });
                    let page_done = body.contains("\"page\":2");
                    let rows = if page_done { serde_json::json!([]) } else { view["rows"].clone() };
                    let payload = serde_json::json!({ "action": "render", "data": { "view": {
                        "columns": view["columns"], "rows": rows } } });
                    return ResponseTemplate::new(200).set_body_string(
                        serde_json::json!({
                            "jsonrpc": "2.0", "id": 2,
                            "result": { "content": [{ "type": "text", "text": payload.to_string() }] }
                        })
                        .to_string(),
                    );
                }
                ResponseTemplate::new(200).set_body_string(
                    r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"unexpected"}}"#,
                )
            })
            .mount(&server)
            .await;

        (server, renamed_to, renames)
    }

    fn title_task() -> SyTask {
        SyTask {
            item_id: ROW_ID.into(),
            title: "таск".into(),
            status: String::new(),
            branch: String::new(),
            pr: String::new(),
            schedule: String::new(),
            last_run: String::new(),
            body_doc: Some(BOUND_DOC.into()),
        }
    }

    fn title_board() -> crate::siyuan_task::SyBoard {
        crate::siyuan_task::SyBoard {
            doc_id: BOARD_DOC.into(),
            av_id: AV_ID.into(),
            block_id: CARRIER.into(),
            hpath: "/uwuwu/Задачи uwuwu".into(),
            notebook: "nb-1".into(),
        }
    }

    fn set_kernel_url(server: &MockServer) {
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn edit_title_renames_doc_and_title_cell_confirms() {
        let (server, _renamed_to, renames) = mock_siyuan_for_title(true).await;
        set_kernel_url(&server);
        let (board, task) = (title_board(), title_task());

        let new_title = edit_title_with_pacing(
            &SiYuan::for_tests(server.uri()),
            &board,
            &task,
            "  новый тайтл  ",
            TitleSyncPacing::instant(),
        )
        .await
        .expect("rename + sync ok");

        assert_eq!(new_title, "новый тайтл", "trim применён, без слэшей");
        let paths = renames.lock().unwrap();
        assert_eq!(
            paths.last().map(String::as_str),
            Some(&format!("/{CHAIN_ROOT}/{BOARD_DOC}/{BOUND_DOC}.sy") as &str),
            "renameDoc по storage-пути из id-цепочки, относительно ноутбука"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn edit_title_bails_on_row_duplicate_without_rename() {
        let (server, _renamed_to, renames) = mock_siyuan_for_title(true).await;
        set_kernel_url(&server);
        let (board, task) = (title_board(), title_task());

        let err = edit_title_with_pacing(
            &SiYuan::for_tests(server.uri()),
            &board,
            &task,
            OTHER_TITLE,
            TitleSyncPacing::instant(),
        )
        .await
        .expect_err("чужой тайтл — отказ до мутаций");

        assert!(
            err.to_string().contains("занят другой строкой"),
            "суть: {err}"
        );
        assert!(renames.lock().unwrap().is_empty(), "renameDoc не вызван");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn edit_title_bails_on_orphan_body_with_target_title() {
        let (server, _renamed_to, renames) = mock_siyuan_for_title(true).await;
        set_kernel_url(&server);
        let (board, task) = (title_board(), title_task());

        let err = edit_title_with_pacing(
            &SiYuan::for_tests(server.uri()),
            &board,
            &task,
            ORPHAN_TITLE,
            TitleSyncPacing::instant(),
        )
        .await
        .expect_err("тело-сирота с целевым тайтлом — отказ");

        assert!(err.to_string().contains("уже существует"), "суть: {err}");
        assert!(renames.lock().unwrap().is_empty(), "renameDoc не вызван");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn edit_title_reports_loudly_when_title_cell_never_syncs() {
        let (server, _renamed_to, renames) = mock_siyuan_for_title(false).await;
        set_kernel_url(&server);
        let (board, task) = (title_board(), title_task());

        let err = edit_title_with_pacing(
            &SiYuan::for_tests(server.uri()),
            &board,
            &task,
            "новый тайтл",
            TitleSyncPacing::instant(),
        )
        .await
        .expect_err("клетка не синкалась — громкая ошибка");

        let text = err.to_string();
        assert!(text.contains("Title-клетка"), "суть: {text}");
        assert!(text.contains("идемпотентен"), "хинт на повтор: {text}");
        assert_eq!(renames.lock().unwrap().len(), 1, "renameDoc был один");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn edit_title_refuses_detached_row_before_any_call() {
        let (server, _renamed_to, renames) = mock_siyuan_for_title(true).await;
        set_kernel_url(&server);
        let mut task = title_task();
        task.body_doc = None;

        let err = edit_title_with_pacing(
            &SiYuan::for_tests(server.uri()),
            &title_board(),
            &task,
            "новый тайтл",
            TitleSyncPacing::instant(),
        )
        .await
        .expect_err("detached-строка — отказ");

        assert!(err.to_string().contains("без тела"), "суть: {err}");
        assert!(renames.lock().unwrap().is_empty(), "renameDoc не вызван");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn edit_title_retry_after_lag_passes_when_own_doc_already_renamed() {
        // retry после неподтвердившегося синка: rename прошёл (док уже «новый
        // тайтл», hpath-лукап отдаёт собственный docId), клетка ещё держит
        // старый тайтл (task.title = «таск»). Повтор обязан пройти, а не
        // советовать «вычисти сироту» про живое тело
        let (server, renamed_to, renames) = mock_siyuan_for_title(true).await;
        set_kernel_url(&server);
        *renamed_to.lock().unwrap() = Some("новый тайтл".into());
        let mut task = title_task();
        task.title = "таск".into();

        let result = edit_title_with_pacing(
            &SiYuan::for_tests(server.uri()),
            &title_board(),
            &task,
            "новый тайтл",
            TitleSyncPacing::instant(),
        )
        .await
        .expect("retry идемпотентен: собственный док исключён из orphan-чека");

        assert_eq!(result, "новый тайтл");
        assert_eq!(
            renames.lock().unwrap().len(),
            1,
            "retry делает свой no-op renameDoc"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn edit_title_same_title_short_circuits_without_kernel_calls() {
        let (server, _renamed_to, renames) = mock_siyuan_for_title(true).await;
        set_kernel_url(&server);
        let task = title_task();

        let result = edit_title_with_pacing(
            &SiYuan::for_tests(server.uri()),
            &title_board(),
            &task,
            "таск",
            TitleSyncPacing::instant(),
        )
        .await
        .expect("no-op rename — успех");

        assert_eq!(result, "таск");
        assert!(renames.lock().unwrap().is_empty(), "ядро не трогается");
    }

    // ——— create --attach: порядок upload → create → append, пред-чек дедупа ———

    /// Мок под полный create-путь: дедуп-пред-чек читает render, createDocWithMd
    /// и asset/upload пишут в общий лог порядка, render stateful (строка доски
    /// появляется после item_add — так живёт verify create).
    async fn mock_siyuan_for_create(
        initial_row_title: Option<&str>,
    ) -> (
        MockServer,
        Arc<Mutex<Vec<&'static str>>>,
        Arc<Mutex<Vec<String>>>,
    ) {
        let server = MockServer::start().await;
        let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let appends: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let item_added: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
        let initial_title = initial_row_title.map(String::from);

        Mock::given(method("POST"))
            .and(path("/api/asset/upload"))
            .respond_with({
                let order = order.clone();
                move |req: &wiremock::Request| {
                    order.lock().unwrap().push("upload");
                    // kernel вставляет stem оригинала в сгенерированный путь:
                    // assets/<stem>-<timestamp>-<random>.<ext>
                    let body = String::from_utf8_lossy(&req.body);
                    let name = body
                        .split("filename=\"")
                        .nth(1)
                        .and_then(|rest| rest.split('"').next())
                        .unwrap_or("f");
                    let (stem, ext) = match name.rsplit_once('.') {
                        Some((stem, ext)) => (stem.to_string(), ext.to_string()),
                        None => (name.to_string(), "bin".to_string()),
                    };
                    ResponseTemplate::new(200).set_body_json(json!({
                        "code": 0,
                        "data": { "succFiles": [{ "index": 0, "name": name,
                                  "path": format!("assets/{stem}-20261106090000-abc1234.{ext}") }] }
                    }))
                }
            })
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/notebook/lsNotebooks"))
            .respond_with(|_req: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_string(
                    json!({ "code": 0, "data": { "notebooks": [{ "id": "nb-1", "name": "Projects" }] } })
                        .to_string(),
                )
            })
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/query/sql"))
            .respond_with(|req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let stmt = body.get("stmt").and_then(|s| s.as_str()).unwrap_or("");
                let data = if stmt.contains("type='av'") {
                    serde_json::json!([{ "id": CARRIER,
                        "markdown": format!("<div data-type=\"NodeAttributeView\" data-av-id=\"{AV_ID}\"></div>") }])
                } else {
                    serde_json::json!([])
                };
                ResponseTemplate::new(200).set_body_string(
                    serde_json::json!({ "code": 0, "data": data }).to_string(),
                )
            })
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/filetree/getIDsByHPath"))
            .respond_with(|req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let hpath = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
                let data = if hpath == "/uwuwu/Задачи uwuwu" {
                    serde_json::json!([BOARD_DOC])
                } else {
                    serde_json::json!([])
                };
                ResponseTemplate::new(200)
                    .set_body_string(serde_json::json!({ "code": 0, "data": data }).to_string())
            })
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/filetree/createDocWithMd"))
            .respond_with({
                let order = order.clone();
                move |_req: &wiremock::Request| {
                    order.lock().unwrap().push("create");
                    ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":"doc-new"}"#)
                }
            })
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/block/appendBlock"))
            .respond_with({
                let order = order.clone();
                let appends = appends.clone();
                move |req: &wiremock::Request| {
                    order.lock().unwrap().push("append");
                    appends
                        .lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&req.body).to_string());
                    ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":null}"#)
                }
            })
            .mount(&server)
            .await;

        let order_for_mcp = order.clone();
        let item_added_for_mcp = item_added.clone();
        let initial_for_mcp = initial_title.clone();
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(move |req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("\"initialize\"") {
                    return ResponseTemplate::new(200)
                        .set_body_string(
                            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"3"}}}"#,
                        )
                        .append_header("mcp-session-id", "sess-1");
                }
                if body.contains("notifications/initialized") {
                    return ResponseTemplate::new(200);
                }
                if body.contains("tools/call") {
                    let action = if body.contains("\"item_add\"") {
                        *item_added_for_mcp.lock().unwrap() = true;
                        order_for_mcp.lock().unwrap().push("item_add");
                        json!({ "action": "item_add", "data": { "itemID": ROW_ID } }).to_string()
                    } else if body.contains("\"keys\"") {
                        json!({ "action": "keys",
                                "data": { "keys": [{ "id": TITLE_KEY, "name": "Status" }] } }).to_string()
                    } else if body.contains("\"render\"") {
                        let page_done = body.contains("\"page\":2");
                        let existing = initial_for_mcp.clone().map(|t| serde_json::json!({
                            "id": OTHER_ROW,
                            "cells": [{ "value": { "keyID": TITLE_KEY, "type": "block",
                                       "block": { "content": t, "id": ORPHAN_DOC, "refSubtype": "d" } } }]
                        }));
                        let mut rows: Vec<Value> = Vec::new();
                        if let Some(row) = existing {
                            rows.push(row);
                        }
                        if *item_added_for_mcp.lock().unwrap() && !page_done {
                            rows.push(serde_json::json!({
                                "id": ROW_ID,
                                "cells": [{ "value": { "keyID": TITLE_KEY, "type": "block",
                                           "block": { "content": "новая задача", "id": "doc-new", "refSubtype": "d" } } }]
                            }));
                        }
                        let rows = if page_done { json!([]) } else { serde_json::Value::Array(rows) };
                        json!({ "action": "render", "data": { "view": {
                            "columns": [{ "id": TITLE_KEY, "name": "Primary Key" }],
                            "rows": rows } } }).to_string()
                    } else {
                        json!({ "action": "unknown", "data": null }).to_string()
                    };
                    return ResponseTemplate::new(200).set_body_string(
                        serde_json::json!({
                            "jsonrpc": "2.0", "id": 2,
                            "result": { "content": [{ "type": "text", "text": action }] }
                        })
                        .to_string(),
                    );
                }
                ResponseTemplate::new(200).set_body_string(
                    r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"unexpected"}}"#,
                )
            })
            .mount(&server)
            .await;

        (server, order, appends)
    }

    async fn create_via_cli(
        server: &MockServer,
        title: &str,
        attach: &[std::path::PathBuf],
    ) -> anyhow::Result<()> {
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
        task_run(
            "uwuwu",
            TaskSub::Create {
                title: title.into(),
                r#type: None,
                stdin: false,
                attach: attach.to_vec(),
                dry_run: false,
            },
        )
        .await
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn create_with_attach_uploads_before_creating_and_appends_refs() {
        let (server, order, appends) = mock_siyuan_for_create(None).await;
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("скрин 1.png");
        std::fs::write(&img, b"png").unwrap();
        let doc = dir.path().join("notes.txt");
        std::fs::write(&doc, b"txt").unwrap();

        create_via_cli(&server, "новая задача", &[img, doc])
            .await
            .expect("create с аттачами ok");

        assert_eq!(
            *order.lock().unwrap(),
            vec!["upload", "upload", "create", "item_add", "append", "append"],
            "порядок: upload все → createDoc → строка → тег типа → референсы"
        );
        let appends = appends.lock().unwrap();
        assert!(
            appends[0].contains("#dev"),
            "первый append — тег типа: {}",
            appends[0]
        );
        let refs_md: String = serde_json::from_str::<serde_json::Value>(&appends[1])
            .expect("appendBlock body — json")
            .get("data")
            .and_then(|d| d.as_str())
            .expect("data — markdown референсов")
            .to_string();
        assert!(
            refs_md.starts_with('\n'),
            "ведущая пустая строка: {refs_md:?}"
        );
        // round-trip через штатного читателя: закодированные пути декодируются
        assert_eq!(
            collect_asset_refs(&refs_md),
            vec![
                "скрин 1-20261106090000-abc1234.png".to_string(),
                "notes-20261106090000-abc1234.txt".to_string(),
            ],
            "обе ссылки читаются collect_asset_refs: {refs_md:?}"
        );
        assert!(
            refs_md.contains("![скрин 1.png]("),
            "картинка по расширению: {refs_md:?}"
        );
        assert!(
            !refs_md.contains("![notes.txt]"),
            "txt — ссылка, не картинка: {refs_md:?}"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn create_with_attach_bails_on_duplicate_title_before_any_upload() {
        let (server, order, _appends) = mock_siyuan_for_create(Some("новая задача")).await;
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("скрин.png");
        std::fs::write(&img, b"png").unwrap();

        let err = create_via_cli(&server, "новая задача", &[img])
            .await
            .expect_err("дубль-тайтл — отказ");

        assert!(
            err.to_string().contains("уже есть"),
            "пред-чек дедупа: {err}"
        );
        assert!(
            !order.lock().unwrap().contains(&"upload"),
            "ни одного upload до отказа"
        );
        assert!(
            !order.lock().unwrap().contains(&"create"),
            "createDoc не вызван"
        );
    }

    // ——— body --attach: порядок upload → контент → референсы; attach-only ———

    /// Мок под body-путь: bound-строка «таск» (resolve id-first), upload и
    /// appendBlock пишут в общий лог порядка, createDocWithMd фиксируется
    /// (append-режим не должен его звать).
    async fn mock_siyuan_for_body() -> (
        MockServer,
        Arc<Mutex<Vec<&'static str>>>,
        Arc<Mutex<Vec<String>>>,
    ) {
        let server = MockServer::start().await;
        let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let appends: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        Mock::given(method("POST"))
            .and(path("/api/asset/upload"))
            .respond_with({
                let order = order.clone();
                move |req: &wiremock::Request| {
                    order.lock().unwrap().push("upload");
                    let body = String::from_utf8_lossy(&req.body);
                    let name = body
                        .split("filename=\"")
                        .nth(1)
                        .and_then(|rest| rest.split('"').next())
                        .unwrap_or("f");
                    let (stem, ext) = match name.rsplit_once('.') {
                        Some((stem, ext)) => (stem.to_string(), ext.to_string()),
                        None => (name.to_string(), "bin".to_string()),
                    };
                    ResponseTemplate::new(200).set_body_json(json!({
                        "code": 0,
                        "data": { "succFiles": [{ "index": 0, "name": name,
                                  "path": format!("assets/{stem}-20261106090000-abc1234.{ext}") }] }
                    }))
                }
            })
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/notebook/lsNotebooks"))
            .respond_with(|_req: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_string(
                    json!({ "code": 0, "data": { "notebooks": [{ "id": "nb-1", "name": "Projects" }] } })
                        .to_string(),
                )
            })
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/query/sql"))
            .respond_with(|req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let stmt = body.get("stmt").and_then(|s| s.as_str()).unwrap_or("");
                let data = if stmt.contains("type='av'") {
                    serde_json::json!([{ "id": CARRIER,
                        "markdown": format!("<div data-type=\"NodeAttributeView\" data-av-id=\"{AV_ID}\"></div>") }])
                } else {
                    serde_json::json!([])
                };
                ResponseTemplate::new(200).set_body_string(
                    serde_json::json!({ "code": 0, "data": data }).to_string(),
                )
            })
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/filetree/getIDsByHPath"))
            .respond_with(|req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let hpath = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
                let data = if hpath == "/uwuwu/Задачи uwuwu" {
                    serde_json::json!([BOARD_DOC])
                } else {
                    serde_json::json!([])
                };
                ResponseTemplate::new(200)
                    .set_body_string(serde_json::json!({ "code": 0, "data": data }).to_string())
            })
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/filetree/createDocWithMd"))
            .respond_with({
                let order = order.clone();
                move |_req: &wiremock::Request| {
                    order.lock().unwrap().push("create");
                    ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":"doc-new"}"#)
                }
            })
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/block/appendBlock"))
            .respond_with({
                let order = order.clone();
                let appends = appends.clone();
                move |req: &wiremock::Request| {
                    order.lock().unwrap().push("append");
                    appends
                        .lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&req.body).to_string());
                    ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":null}"#)
                }
            })
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(|req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("\"initialize\"") {
                    return ResponseTemplate::new(200)
                        .set_body_string(
                            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"3"}}}"#,
                        )
                        .append_header("mcp-session-id", "sess-1");
                }
                if body.contains("notifications/initialized") {
                    return ResponseTemplate::new(200);
                }
                if body.contains("tools/call") {
                    let payload = if body.contains("\"block\"") && body.contains("\"get\"") {
                        // doc_block_exists: bound-док жив
                        format!("ID: {BOUND_DOC}\nType: NodeDocument\nHPath: /uwuwu/Задачи uwuwu/таск")
                    } else if body.contains("\"render\"") {
                        let page_done = body.contains("\"page\":2");
                        let rows = if page_done {
                            json!([])
                        } else {
                            json!([{ "id": ROW_ID, "cells": [{ "value": { "keyID": TITLE_KEY,
                                "type": "block",
                                "block": { "content": "таск", "id": BOUND_DOC, "refSubtype": "d" } } }] }])
                        };
                        json!({ "action": "render", "data": { "view": {
                            "columns": [{ "id": TITLE_KEY, "name": "Primary Key" }],
                            "rows": rows } } }).to_string()
                    } else {
                        json!({ "action": "unknown", "data": null }).to_string()
                    };
                    return ResponseTemplate::new(200).set_body_string(
                        serde_json::json!({
                            "jsonrpc": "2.0", "id": 2,
                            "result": { "content": [{ "type": "text", "text": payload }] }
                        })
                        .to_string(),
                    );
                }
                ResponseTemplate::new(200).set_body_string(
                    r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"unexpected"}}"#,
                )
            })
            .mount(&server)
            .await;

        (server, order, appends)
    }

    async fn body_via_cli(
        server: &MockServer,
        md: Option<&std::path::Path>,
        attach: &[std::path::PathBuf],
    ) -> anyhow::Result<()> {
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
        task_run(
            "uwuwu",
            TaskSub::Body {
                id: "таск".into(),
                stdin: false,
                md: md.map(|p| p.to_string_lossy().into_owned()),
                replace: false,
                replace_section: None,
                attach: attach.to_vec(),
            },
        )
        .await
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn body_with_attach_uploads_then_content_then_refs() {
        let (server, order, appends) = mock_siyuan_for_body().await;
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("скрин до.png");
        std::fs::write(&img, b"png").unwrap();
        let md = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(md.path(), "маркер-контент\n").unwrap();

        body_via_cli(&server, Some(md.path()), &[img])
            .await
            .expect("body --attach с контентом ok");

        assert_eq!(
            *order.lock().unwrap(),
            vec!["upload", "append", "append"],
            "порядок: upload → контент-операция → референсы; createDoc не зовётся"
        );
        let appends = appends.lock().unwrap();
        assert!(
            appends[0].contains("маркер-контент"),
            "первый append — контент: {}",
            appends[0]
        );
        assert!(
            appends[1].contains("![скрин до.png](assets/"),
            "второй append — референс картинки: {}",
            appends[1]
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn body_attach_only_skips_empty_content_write_and_appends_refs() {
        let (server, order, appends) = mock_siyuan_for_body().await;
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("после.png");
        std::fs::write(&img, b"png").unwrap();

        body_via_cli(&server, None, &[img])
            .await
            .expect("attach-only вызов валиден");

        assert_eq!(
            *order.lock().unwrap(),
            vec!["upload", "append"],
            "пустой контент-оп не делается: без пустого appendBlock перед референсами"
        );
        let appends = appends.lock().unwrap();
        assert_eq!(appends.len(), 1, "единственный append — референсы");
        assert!(
            !appends[0].contains("\"data\":\"\""),
            "в appendBlock не уходит пустой data: {}",
            appends[0]
        );
        let refs_md: String = serde_json::from_str::<serde_json::Value>(&appends[0])
            .expect("appendBlock body — json")
            .get("data")
            .and_then(|d| d.as_str())
            .expect("data — markdown референсов")
            .to_string();
        assert_eq!(
            collect_asset_refs(&refs_md),
            vec!["после-20261106090000-abc1234.png".to_string()],
            "round-trip референса attach-only: {refs_md:?}"
        );
    }

    /// Golden stdout-головы search (Low ревью PR #78): формат парсят
    /// фабричные агенты — правка println! мимо search_head невозможна,
    /// правка самого search_head красит этот тест.
    #[test]
    fn search_head_is_byte_stable_contract_format() {
        assert_eq!(
            search_head("семантический индекс"),
            "Searching for: \"семантический индекс\""
        );
    }
}
