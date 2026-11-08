//! Handlers всех команд uwuwu-cli на SiYuan-бекенде (единственный).

use super::docs_ls;
use super::read_resolve;
use crate::cmd::cli::{Commands, CronSub, GoalSub, KernelSub, PersonSub, SemanticSub, TaskSub};
use crate::html_embed;
use crate::markdown;
use crate::md_artifacts;
use crate::md_heading;
use crate::siyuan::find_marker_location as sy_find_marker_location;
use crate::siyuan_goal as goal;
use crate::siyuan_person as person;
use crate::siyuan_person::person_target;
use crate::siyuan::{FileFetch, SiYuan};
use crate::siyuan_guard as guard;
use crate::siyuan_guard::{Journal, JournalRow, Probe, Verdict};
use crate::siyuan_task as syt;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

pub(crate) async fn dispatch(command: Commands) -> anyhow::Result<()> {
    match command {
        Commands::Task { command, project } => {
            let project = project.unwrap_or_default();
            task_run(&project, command).await
        }
        Commands::Goal { command, project } => {
            let project = project.unwrap_or_default();
            goal_run(&project, command).await
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
        Commands::Docs { command } => docs_ls::docs_run(command).await,
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

/// Резолв задачи для id-команд: с --project — доска проекта, без — глобально
/// по доскам всех проектов (уникальность обязательна, раскладка «доска →
/// совпадения» в ошибках).
async fn resolve_task(
    sy: &SiYuan,
    project: &str,
    needle: &str,
) -> Result<(syt::SyBoard, syt::SyTask)> {
    if project.is_empty() {
        syt::resolve_global(sy, needle).await
    } else {
        syt::resolve_on_project(sy, project, needle).await
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
        schedule,
        agent,
        dry_run: true,
    } = command
    {
        if !attach.is_empty() {
            anyhow::bail!("create --dry-run не аттачит: скелет печатается без записей в воркспейс — убери --attach");
        }
        // --type валидируется и в осмотре: флаг не молчит (ревью Low)
        let ty = match ty.as_deref() {
            Some(raw) => syt::validate_task_type(raw)?,
            None => "dev".to_string(),
        };
        // --schedule валидируется и в осмотре: флаг не молчит (ревью Low)
        if let Some(expr) = schedule.as_deref() {
            crate::schedule::validate_schedule(expr)?;
            println!("расписание: {expr} (будет записано атрибутом custom-schedule дока-тела)");
        }
        // --agent валидируется и в осмотре: флаг не молчит (паритет schedule)
        if let Some(name) = agent.as_deref() {
            if name.is_empty() {
                anyhow::bail!("на create пустой агент бессмысленен — убери --agent; снятие: task edit <id> --agent \"\"");
            }
            syt::validate_agent_name(name)?;
            println!("агент: {name} (будет записан атрибутом custom-agent дока-тела)");
        }
        println!("тип: {ty} (колонка «{}» доски)", syt::TYPE_CELL);
        let ctx = if stdin { read_stdin()? } else { String::new() };
        println!("{}", syt::body_template(&ty, &ctx));
        return Ok(());
    }
    // Гвард позиционного текста коммента ДО bail по --project: иначе флаг,
    // проглоченный trailing_var_arg'ом (`task comment <id> "текст" --project X`
    // → project=None), маскируется под «флаг не передан» (крон-сессия 04.11).
    // stdin здесь НЕ читается — его читает ровно одна ветка ниже.
    if let TaskSub::Comment { stdin, text, .. } = &command {
        pre_guard_positional_comment(*stdin, text)?;
    }
    // Board-bound подкоманды привязаны к доске/журналу — глобальной семантики
    // нет. Id-семейство (get/edit/claim/comment/thread/close/body/rm) без
    // --project резолвит глобально: resolve_task → resolve_global (доски всех
    // проектов, уникальность обязательна).
    if project.is_empty()
        && matches!(
            command,
            TaskSub::List { .. }
                | TaskSub::Create { .. }
                | TaskSub::Clean { .. }
                | TaskSub::Undo { .. }
        )
    {
        anyhow::bail!("--project <имя> обязателен для list/create/clean/undo (uwuwu | origa | …)");
    }
    let sy = SiYuan::from_env()?;
    match command {
        TaskSub::List {
            status, r#type: ty, ..
        } => {
            // мусорный тип фильтра — громкий отказ через тот же канон,
            // а не тихая пустота «Задач нет.» (паритет create/edit)
            let ty = match ty.as_deref() {
                Some(raw) => Some(syt::validate_task_type(raw)?),
                None => None,
            };
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
                .filter(|r| {
                    ty.as_deref()
                        .map(|t| r.task_type.eq_ignore_ascii_case(t))
                        .unwrap_or(true)
                })
                .collect();
            if filtered.is_empty() {
                println!("Задач нет.");
            }
            println!(
                "{:<13} {:<6} {:<12} {:<34} PR",
                "СТАТУС", "ТИП", "СТРОКА", "ЗАГОЛОВОК"
            );
            filtered.sort_by(|a, b| a.status.cmp(&b.status));
            for r in &filtered {
                let marker = match r.body_doc.as_deref() {
                    Some(b) if dead_bound.contains(b) => " (!)",
                    _ => "",
                };
                println!(
                    "{:<13} {:<6} {:<12} {:<34} {}{}",
                    r.status,
                    r.task_type,
                    &r.item_id[..r.item_id.len().min(12)],
                    truncate(&r.title, 32),
                    r.pr,
                    marker
                );
            }
            Ok(())
        }
        TaskSub::Get { id, no_body } => {
            let (board, task) = resolve_task(&sy, project, &id).await?;
            println!("Задача:   {}", task.title);
            println!("Статус:   {}", task.status);
            if !task.task_type.is_empty() {
                println!("Тип:      {}", task.task_type);
            }
            println!("Строка:   {}", task.item_id);
            // где приземлился резолв — особенно при глобальном (без --project)
            println!("Доска:    {}", board.hpath);
            if !task.branch.is_empty() {
                println!("Ветка:    {}", task.branch);
            }
            if !task.pr.is_empty() {
                println!("PR:       {}", task.pr);
            }
            if !task.planned.is_empty() {
                println!("Запланировано: {}", task.planned);
            }
            let body_doc = syt::task_body_doc(&sy, &board, &task).await?;
            if let Some(doc) = body_doc.as_deref() {
                if let Ok(attrs) = sy.get_block_attrs(doc).await {
                    if let Some(schedule) = attrs.get("custom-schedule") {
                        println!("Расписание: {schedule} (крон: файр в статусе Вейт)");
                    }
                    if let Some(agent) = attrs.get("custom-agent") {
                        println!("{}", syt::agent_display_line(agent));
                    }
                }
            }
            if !no_body {
                let md = match body_doc {
                    Some(doc) => sy.export_md_by_id(&doc).await.unwrap_or_default(),
                    None => String::new(),
                };
                for warning in md_artifacts::artifact_report(&md) {
                    eprintln!("⚠ round-trip-артефакт: {warning}");
                }
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
            schedule,
            agent,
            ..
        } => {
            let attach_files = validate_attach_files(&attach)?;
            let ctx = if stdin { read_stdin()? } else { String::new() };
            // тип валидируется до любых записей: пустой на create
            // бессмысленен (паритет расписания), канон — дефолт dev тоже
            // пишется в колонку
            let ty_canonical = match ty.as_deref() {
                Some("") => anyhow::bail!(
                    "на create пустой тип бессмысленен — убери --type; снятие: task edit <id> --type \"\""
                ),
                Some(raw) => syt::validate_task_type(raw)?,
                None => "dev".to_string(),
            };
            if let Some(expr) = schedule.as_deref() {
                if expr.is_empty() {
                    anyhow::bail!(
                        "на create пустое расписание бессмысленно — убери --schedule; снятие: task edit <id> --schedule \"\""
                    );
                }
                crate::schedule::validate_schedule(expr)?;
            }
            if let Some(name) = agent.as_deref() {
                if name.is_empty() {
                    anyhow::bail!(
                        "на create пустой агент бессмысленен — убери --agent; снятие: task edit <id> --agent \"\""
                    );
                }
                syt::validate_agent_name(name)?;
            }
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
            let (item, body) = syt::create(&sy, project, &title, &ty_canonical, &ctx).await?;
            if let Some(expr) = schedule.as_deref() {
                // сбой после create не теряет задачу: атрибут доставляется
                // повторным task edit --schedule (создание уже свершилось)
                sy.set_block_attr(&body, "custom-schedule", expr)
                    .await
                    .with_context(|| {
                        format!(
                            "задача создана (строка {item}, тело {body}), но расписание \
                             не записано — доставь: task edit {item} --schedule \"{expr}\""
                        )
                    })?;
                // колонка «Расписание» — синхронно с attr (доску create не
                // возвращает; повторный find — две read-пробы, только при --schedule)
                let board = syt::find_board(&sy, project).await.with_context(|| {
                    format!(
                        "задача создана (строка {item}), но клетку «{}» доставь: \
                         task edit {item} --schedule \"{expr}\"",
                        syt::SCHEDULE_CELL
                    )
                })?;
                syt::sync_schedule_cell(&sy, &board, &item, expr).await?;
            }
            if let Some(name) = agent.as_deref() {
                // сбой после create не теряет задачу: атрибут доставляется
                // повторным task edit --agent (создание уже свершилось)
                sy.set_block_attr(&body, "custom-agent", name)
                    .await
                    .with_context(|| {
                        format!(
                            "задача создана (строка {item}, тело {body}), но агент \
                             не записан — доставь: task edit {item} --agent \"{name}\""
                        )
                    })?;
            }
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
            r#type: ty,
            schedule,
            agent,
            planned,
        } => {
            let (board, task) = resolve_task(&sy, project, &id).await?;
            // валидация PR/типа/статуса/planned до rename: без частичных
            // обновлений (тайтл сменился, а колонки нет — недопустимый
            // разрыв); «» — снятие типа/даты
            pr.as_deref().map(syt::pr_cell_value).transpose()?;
            let ty_canonical = match ty.as_deref() {
                Some("") => Some(String::new()),
                Some(raw) => Some(syt::validate_task_type(raw)?),
                None => None,
            };
            // канон-форма (не эхо ввода): ядро апсертит mSelect-опцию по
            // точному content — «ревью плана» mint'нул бы дубль «Ревью плана»
            let status_canonical = status.as_deref().map(syt::validate_status).transpose()?;
            if let Some(raw) = planned.as_deref().filter(|s| !s.is_empty()) {
                syt::parse_planned(raw)?;
            }
            let final_title = match title.as_deref() {
                Some(raw) => Some(syt::edit_title(&sy, &board, &task, raw).await?),
                None => None,
            };
            syt::edit(
                &sy,
                &board,
                &task,
                status_canonical.as_deref(),
                branch.as_deref(),
                pr.as_deref(),
                ty_canonical.as_deref(),
            )
            .await?;
            if let Some(raw) = planned.as_deref() {
                syt::set_planned(&sy, &board, &task, raw).await?;
            }
            if ty_canonical.as_deref() == Some("") {
                println!("тип снят: {}", task.title);
            }
            match planned.as_deref() {
                Some("") => println!("запланировано снято: {}", task.title),
                Some(raw) => {
                    let (_, formatted) = syt::parse_planned(raw)?;
                    println!("запланировано: {formatted}");
                }
                None => {}
            }
            if let Some(expr) = schedule.as_deref() {
                // пустая строка = снятие атрибута (ядро удаляет ключ из IAL);
                // валидация — только для непустого значения: контракт «"" — снять»
                // не должен упираться в отказ грамматики
                if !expr.is_empty() {
                    crate::schedule::validate_schedule(expr)?;
                }
                let body = syt::task_body_doc(&sy, &board, &task)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "строка «{}» без тела (detached) — расписанию некуда записаться",
                            task.title
                        )
                    })?;
                sy.set_block_attr(&body, "custom-schedule", expr).await?;
                // колонка «Расписание» — отображение attr: пишется синхронно
                // в единственной точке записи расписания (снятие чистит клетку)
                syt::sync_schedule_cell(&sy, &board, &task.item_id, expr).await?;
                if expr.is_empty() {
                    println!("расписание снято: {}", task.title);
                }
            }
            if let Some(name) = agent.as_deref() {
                // пустая строка = снятие атрибута (мастер файрится
                // изолированными спавнами); валидация — только непустого
                if !name.is_empty() {
                    syt::validate_agent_name(name)?;
                }
                let body = syt::task_body_doc(&sy, &board, &task)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "строка «{}» без тела (detached) — агенту некуда записаться",
                            task.title
                        )
                    })?;
                sy.set_block_attr(&body, "custom-agent", name).await?;
                if name.is_empty() {
                    println!("агент снят: {}", task.title);
                }
            }
            println!(
                "обновлено: {}",
                final_title.as_deref().unwrap_or(&task.title)
            );
            Ok(())
        }
        TaskSub::Claim { id, session } => {
            let session = match session {
                Some(sid) => sid,
                None => std::env::var("OPENCODE_SESSION_ID").map_err(|_| anyhow!(
                    "sessionID не задан: нет --session и нет OPENCODE_SESSION_ID в env — заявись из сессии агента или передай id явно"
                ))?,
            };
            let (board, task) = resolve_task(&sy, project, &id).await?;
            syt::claim(&sy, &board, &task, &session).await?;
            println!("заявлено: {} ← {}", task.title, session);
            Ok(())
        }
        TaskSub::Comment {
            id,
            reply,
            stdin,
            text,
        } => {
            let msg = guarded_comment_text(stdin, &text)?;
            // write-side превенция тег-матча ядра (см. body): предупреждение
            // в stderr, запись не блокируется
            if let Some(warning) = md_artifacts::tag_trap_warning(&msg) {
                eprintln!("{warning}");
            }
            let (board, task) = resolve_task(&sy, project, &id).await?;
            syt::comment(&sy, &board, &task, "AGENTS", &msg, reply.as_deref()).await?;
            println!(
                "коммент отправлен на {}: {}",
                board.hpath,
                truncate(&msg, 60)
            );
            Ok(())
        }
        TaskSub::Thread { id } => {
            let (board, task) = resolve_task(&sy, project, &id).await?;
            let md = match syt::task_body_doc(&sy, &board, &task).await? {
                Some(doc) => sy.export_md_by_id(&doc).await.unwrap_or_default(),
                None => String::new(),
            };
            print_threads(&md);
            Ok(())
        }
        TaskSub::Close { id, note } => {
            let (board, task) = resolve_task(&sy, project, &id).await?;
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
            base,
            force,
        } => {
            let attach_files = validate_attach_files(&attach)?;
            let content = read_body_input(md.as_deref(), stdin, !attach_files.is_empty())?;
            // гард вставки обёртки вывода task get в контент (202611080031):
            // вход пишется как есть — шапка карточки, маркер и frontmatter от
            // get-вывода ложатся в тело литерально и множатся на replace-циклах.
            // Отказ до сети и записи (no-write); при --force тоже — санитария
            // входа, инвариант, а не часть сверки свежести. Снапшот --base
            // допустим полным: cli срезает обёртку при сверке сам.
            if let Some(signature) = md_artifacts::task_get_wrapper_signature(&content) {
                bail!(
                    "вход несёт обёртку вывода task get ({signature}) — она легла бы в тело \
                     литерально и множилась на replace-циклах. Скармливай контент тела: без \
                     шапки карточки «Задача:/Статус:/Строка:…», маркера «----- тело -----» и \
                     frontmatter — frontmatter дока ведёт ядро, титульный H1 пишется из \
                     заголовка дока. Снапшот для --base можно не чистить: cli срезает обёртку \
                     при сверке сам"
                );
            }
            // write-side превенция тег-матча ядра: голые решётки с пунктуацией
            // вплотную съедают бэктики код-спанов на экспорте; предупреждение
            // не блокирует запись (легитимные тела О самой порче триггерят)
            if let Some(warning) = md_artifacts::tag_trap_warning(&content) {
                eprintln!("{warning}");
            }
            // фенс-вход валидируется до resolve/upload: отказ (дубль имени,
            // незакрытый фенс, пустая строка в html) не грузит ассеты
            let fences = html_embed::parse_input(&content)?;
            // снапшот читается до обращения к ядру: локальная ошибка — fail fast
            let snapshot = match base.as_deref() {
                Some(path) => Some(
                    std::fs::read_to_string(path)
                        .with_context(|| format!("снапшот --base не читается: {path}"))?,
                ),
                None => None,
            };
            let (board, task) = resolve_task(&sy, project, &id).await?;
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
                    // ядра на пустой data — референсы дописываются сами.
                    // Ввод только из named-фенсов — легитимная запись без
                    // residual-текста («замени диаграмму arch»).
                    let rewrite = if replace || replace_section.is_some() {
                        Some(BodyRewrite {
                            section: replace_section.as_deref(),
                            snapshot: snapshot.as_deref(),
                            force,
                        })
                    } else {
                        None
                    };
                    if rewrite.is_some()
                        || !content.trim().is_empty()
                        || !fences.named_fences().is_empty()
                    {
                        write_body_in_place(&sy, &doc_id, &task.title, rewrite, &content).await?;
                    }
                    doc_id
                }
                None => {
                    // тела нет вовсе: создаю по заголовку (не привязываю — itemID строки неприкосновенен)
                    // титульный H1 стрипается: createDocWithMd оставил бы его
                    // heading-блоком — док родился бы с дублем к заголовку
                    let initial = if replace || replace_section.is_some() {
                        fences.inline_md()
                    } else {
                        fences.inline_md().trim_start().to_string()
                    };
                    let initial = markdown::strip_title_h1(&initial, &task.title);
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

// ——— goal: реестр долгосрочных целей («Цели <proj>») ———

/// Диспетчер цели-семьи: доску «Цели <proj>» заводит владелец руками в UI —
/// CLI без неё громко отказывается с инструкцией (find_goal_board).
pub(crate) async fn goal_run(project: &str, command: GoalSub) -> anyhow::Result<()> {
    if project.is_empty() {
        anyhow::bail!("--project <имя> обязателен (uwuwu | origa | …)");
    }
    let sy = SiYuan::from_env()?;
    match command {
        GoalSub::List { status } => {
            let board = goal::find_goal_board(&sy, project).await?;
            let rows = syt::read_rows(&sy, &board).await?;
            let dead_bound = syt::dead_bound_doc_ids(&sy, &rows).await?;
            let filtered: Vec<_> = rows
                .into_iter()
                .filter(|r| {
                    status
                        .as_deref()
                        .map(|s| r.status.to_lowercase().starts_with(&s.to_lowercase()))
                        .unwrap_or(true)
                })
                .collect();
            let table = format_goal_table(&filtered, &dead_bound);
            println!("{table}");
            Ok(())
        }
        GoalSub::Add {
            title,
            status,
            stdin,
        } => {
            if title.trim().is_empty() {
                anyhow::bail!("пустой тайтл — передай непустую строку в --title");
            }
            let ctx = if stdin { read_stdin()? } else { String::new() };
            let canonical = status.as_deref().map(goal::canonical_status).transpose()?;
            let (item, body) = goal::create(
                &sy,
                project,
                &title,
                canonical.as_deref().unwrap_or("todo"),
                &ctx,
            )
            .await?;
            println!("создана цель: {title}\n  строка: {item}\n  тело:   {body}");
            Ok(())
        }
        GoalSub::Edit { id, status, title } => {
            if status.is_none() && title.is_none() {
                anyhow::bail!("нечего менять: передай --status и/или --title");
            }
            let board = goal::find_goal_board(&sy, project).await?;
            let goal = syt::resolve_entity(&sy, &board, &id, "цель").await?;
            // канонизация статуса до любых записей: без частичных обновлений
            let canonical = status.as_deref().map(goal::canonical_status).transpose()?;
            if let Some(st) = canonical.as_deref() {
                syt::edit(&sy, &board, &goal, Some(st), None, None, None).await?;
            }
            if let Some(raw) = title.as_deref() {
                syt::edit_title(&sy, &board, &goal, raw).await?;
            }
            println!("обновлена цель: {}", goal.title);
            Ok(())
        }
        GoalSub::Rm { id } => goal_rm(&sy, project, &id).await,
    }
}

/// Таблица реестра целей: статус → строка → заголовок; `(!)` — мёртвая
/// привязка тела (канон task list). Строки сортируются по статусу
/// (группировка канбана: done-цели не теряются среди активных).
pub(crate) fn format_goal_table(
    rows: &[syt::SyTask],
    dead_bound: &std::collections::HashSet<String>,
) -> String {
    if rows.is_empty() {
        return "Целей нет.".to_string();
    }
    let mut sorted: Vec<&syt::SyTask> = rows.iter().collect();
    // канонический порядок статусов (лестница прогресса todo → in progress →
    // done), не алфавит: группы канбана читаются сверху вниз по фазе цели;
    // вне-канонные статусы — в конец
    sorted.sort_by_key(|r| {
        goal::GOAL_STATUSES
            .iter()
            .position(|canon| canon.eq_ignore_ascii_case(&r.status))
            .unwrap_or(goal::GOAL_STATUSES.len())
    });
    let mut out = String::new();
    out.push_str(&format!(
        "{:<13} {:<12} {:<34}\n",
        "СТАТУС", "СТРОКА", "ЦЕЛЬ"
    ));
    for r in sorted {
        let marker = match r.body_doc.as_deref() {
            Some(b) if dead_bound.contains(b) => " (!)",
            _ => "",
        };
        out.push_str(&format!(
            "{:<13} {:<12} {:<34}{}\n",
            r.status,
            &r.item_id[..r.item_id.len().min(12)],
            truncate(&r.title, 32),
            marker
        ));
    }
    out
}

/// Снос цели: write-ahead undo-журнал → item_remove строки → тело в трэш
/// (паттерн cron rm; Draft-вердикт task rm тут не нужен — цели всегда Live).
async fn goal_rm(sy: &SiYuan, project: &str, id: &str) -> anyhow::Result<()> {
    let board = goal::find_goal_board(sy, project).await?;
    let goal = syt::resolve_entity(sy, &board, id, "цель").await?;
    let mut journal = Journal::new(project, "goal-rm", &board);
    journal.rows.push(journal_row(sy, &board, &goal).await?);
    let journal_path = journal.save(sy).await?;
    sy.mcp_tool(
        "database",
        json!({ "action": "item_remove", "id": board.av_id, "itemIDs": [goal.item_id] }),
    )
    .await?;
    if let Some(doc) = syt::task_body_doc(sy, &board, &goal).await? {
        let _ = sy.remove_doc(&doc).await;
    }
    println!("удалена цель: {} (undo-журнал: {journal_path})", goal.title);
    Ok(())
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
    Ok(JournalRow {
        item_id: task.item_id.clone(),
        title: task.title.clone(),
        status: task.status.clone(),
        branch: task.branch.clone(),
        pr: task.pr.clone(),
        extra_text_cells: Vec::new(),
        body_doc: task.body_doc.clone(),
        body_hpath: board.body_hpath(&task.title),
        body_md,
    })
}

/// rm одной задачи под guard'ом: вердикт по источникам, отказ для живых
/// не-Draft (задекларированный контракт команды), write-ahead журнал.
/// Id-команда: без --project резолвит глобально; имя проекта для журнала
/// при этом выводится из доски (undo-валидация остаётся проектной).
async fn task_rm(sy: &SiYuan, project: &str, id: &str) -> Result<()> {
    let (board, task) = resolve_task(sy, project, id).await?;
    let project = if project.is_empty() {
        syt::board_project(&board)
    } else {
        project.to_string()
    };
    let chain = syt::board_doc_chain(sy, &board).await?;
    // Строка резолвилась на доске — повторный resolve не нужен: вердикт
    // строится по найденной задаче (resolve внутри task_rm дублировал бы
    // render; контракт guard'а — вердикт по независимым источникам, не
    // по повторному матчингу).
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
    let mut journal = Journal::new(&project, "rm", &board);
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
                planned.push((frag, *task, verdict));
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
        // цели: доска «Цели <proj>» (владелец завёл один раз) — не доска задач
        "goal-rm" => goal::find_goal_board(sy, project).await?,
        // легаси "cron-rm" (крон-доски сняты) уводится на задачную доску
        // и честно падает по av-mismatch ниже — восстанавливать вручную
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
        restore_journal_cells(sy, &board, &item, row).await?;
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

/// Клетки восстановленной строки: Status (mSelect) + Branch + PR (url).
/// Легаси "cron-rm"-журналы (крон-доски сняты) не восстанавливаются:
/// undo уводится на задачную доску и честно падает по av-mismatch.
async fn restore_journal_cells(
    sy: &SiYuan,
    board: &syt::SyBoard,
    item: &str,
    row: &JournalRow,
) -> Result<()> {
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

// ——— cron: инвентарь крон-задач (мастера с расписанием на доске задач) ———

pub(crate) async fn cron_run(command: CronSub) -> Result<()> {
    let sy = SiYuan::from_env()?;
    let CronSub::List { project } = command;
    cron_list(&sy, project.as_deref().unwrap_or("uwuwu")).await
}

/// State демона uwuwu-cron: пер-машинский файл (~/.local/state/uwuwu-cron,
/// песочница двигает UWUWU_CRON_STATE_DIR). Отсутствует/бит — пусто: инвентарь
/// живёт на доске, state только дополняет last run/сессией.
#[derive(serde::Deserialize, Default)]
struct CronStateFile {
    #[serde(default)]
    crons: std::collections::HashMap<String, TrackedCronEntry>,
}

#[derive(serde::Deserialize)]
struct TrackedCronEntry {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    last_fired_at: u64,
}

fn read_cron_state() -> CronStateFile {
    // env — песочница: читается ТОЛЬКО она (изоляция тестов)
    if let Ok(dir) = std::env::var("UWUWU_CRON_STATE_DIR") {
        return read_state_file(std::path::PathBuf::from(dir).join("state.json"))
            .unwrap_or_default();
    }
    // демон (master-чекаут) пишет прод-state; веточный чекаут демона —
    // branches/<ветка>/ (изоляция resolveDaemonStateDir): мержим все,
    // по строке побеждает свежайший lastFiredAt (живых веток обычно одна)
    let base = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".local/state/uwuwu-cron");
    let mut merged = read_state_file(base.join("state.json")).unwrap_or_default();
    if let Ok(entries) = std::fs::read_dir(base.join("branches")) {
        for entry in entries.flatten() {
            let Some(other) = read_state_file(entry.path().join("state.json")).ok() else {
                continue;
            };
            for (row, t) in other.crons {
                let keep = merged
                    .crons
                    .get(&row)
                    .map(|cur| t.last_fired_at > cur.last_fired_at)
                    .unwrap_or(true);
                if keep {
                    merged.crons.insert(row, t);
                }
            }
        }
    }
    merged
}

fn read_state_file(path: std::path::PathBuf) -> Result<CronStateFile> {
    let raw = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

/// Локальное время «YYYY-MM-DD HH:MM» из unix-ms — формат ячейки Last Run
/// демона (читается юзером, не UTC). pub(crate) для тестов cron list.
pub(crate) fn format_local(unix_ms: u64) -> String {
    use chrono::TimeZone;
    let dt = chrono::Local
        .timestamp_millis_opt(unix_ms as i64)
        .single()
        .unwrap_or_default();
    dt.format("%Y-%m-%d %H:%M").to_string()
}

/// Инвентарь мастеров крон-задач доски проекта: строки с custom-schedule
/// в доке-теле. Скедул — атрибут, last run/сессия — state демона.
async fn cron_list(sy: &SiYuan, project: &str) -> Result<()> {
    let board = syt::find_board(sy, project).await?;
    let rows = syt::read_rows(sy, &board).await?;
    let state = read_cron_state();
    let mut masters: Vec<(String, String, String, String, String, String)> = Vec::new();
    for r in &rows {
        let Some(doc) = r.body_doc.as_deref() else {
            continue;
        };
        let attrs = match sy.get_block_attrs(doc).await {
            Ok(a) => a,
            // мёртвая привязка/сбой одной строки не роняет инвентарь:
            // строка пропускается с предупреждением (честность = stderr-нота,
            // не отказ всей команды)
            Err(e) => {
                eprintln!(
                    "  ⚠ «{}» ({doc}): атрибуты не прочитаны ({e}) — строка пропущена",
                    r.title
                );
                continue;
            }
        };
        let Some(schedule) = attrs.get("custom-schedule") else {
            continue;
        };
        let agent = syt::agent_inventory_cell(&attrs);
        let last_run = state
            .crons
            .get(&r.item_id)
            .filter(|t| t.last_fired_at > 0)
            .map(|t| {
                let session = if t.session_id.is_empty() {
                    String::new()
                } else {
                    format!(" {}", &t.session_id[..t.session_id.len().min(10)])
                };
                format!("{}{}", format_local(t.last_fired_at), session)
            })
            .unwrap_or_default();
        masters.push((
            r.status.clone(),
            r.item_id.chars().take(12).collect(),
            truncate(&r.title, 26),
            truncate(schedule, 13),
            truncate(&agent, 12),
            last_run,
        ));
    }
    if masters.is_empty() {
        println!("Крон-задач нет (расписание — task create/edit --schedule, статус Вейт).");
        return Ok(());
    }
    println!(
        "{:<10} {:<12} {:<28} {:<14} {:<13} LAST RUN",
        "СТАТУС", "СТРОКА", "ЗАГОЛОВОК", "SCHEDULE", "AGENT"
    );
    for m in masters {
        println!(
            "{:<10} {:<12} {:<28} {:<14} {:<13} {}",
            m.0, m.1, m.2, m.3, m.4, m.5
        );
    }
    Ok(())
}

/// Одна text-клетка (keyID от board_keys).
async fn write_text_cell(sy: &SiYuan, av: &str, key: &str, item: &str, value: &str) -> Result<()> {
    sy.mcp_tool(
        "database",
        json!({ "action": "item_update", "id": av, "keyID": key, "itemID": item,
                "value": { "text": { "content": value } } }),
    )
    .await?;
    Ok(())
}

/// Текстовые клетки по именам колонок. Отсутствующая колонка — ошибка:
/// тихий скип маскировал незаписанную ячейку (репро 04.11, задача 20261104220156).
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

// ——— docs/search/blob/projects ———

/// Голова stdout search — контракт фабричных агентов (парсят вывод);
/// единственная точка формата, правка = красный golden-тест.
fn search_head(query: &str) -> String {
    format!("Searching for: \"{query}\"")
}

/// Потолок фетча доков для doc-секции: запас от текущего размера
/// воркспейса (1642 дока на 07.11); свыше — тихий пропуск хвоста.
const DOC_TITLES_CAP: usize = 5000;

/// Заголовок doc-секции search.
const DOC_SECTION_HEADER: &str = "Доки (по hpath):";

/// Строки полнотекстовой секции — формат байт-в-байт прежний:
/// вывод парсят фабричные агенты (контракт регрессии 20261104072044,
/// golden-тест локает).
pub(super) fn fulltext_lines(rows: &[Value]) -> Vec<String> {
    let mut lines = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let hpath = r.get("hpath").and_then(|h| h.as_str()).unwrap_or("");
        let content = r.get("content").and_then(|c| c.as_str()).unwrap_or("");
        lines.push(format!("{}. {}", i + 1, hpath));
        lines.push(format!("    {}", content.replace('\n', " ")));
    }
    if rows.is_empty() {
        lines.push("ничего не найдено".to_string());
    }
    lines
}

/// Секция «доки по hpath»: заголовки, которых контентный like не видит.
/// Матч в Rust — SQLite LIKE регистрозависим для кириллицы (проба 07.11:
/// lowercase-запрос даёт пусто). Ранжирование: точный подстрочный матч
/// всего запроса раньше AND-токенов; пустой матч → секция не печатается.
pub(super) fn doc_section_lines(query: &str, hpaths: &[String], limit: usize) -> Vec<String> {
    let tokens: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    let whole_query = tokens.join(" ");
    let mut exact = Vec::new();
    let mut by_tokens = Vec::new();
    for hpath in hpaths {
        let lower = hpath.to_lowercase();
        if lower.contains(&whole_query) {
            exact.push(hpath);
        } else if tokens.iter().all(|t| lower.contains(t.as_str())) {
            by_tokens.push(hpath);
        }
    }
    let hits: Vec<&String> = exact.into_iter().chain(by_tokens).take(limit).collect();
    if hits.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![DOC_SECTION_HEADER.to_string()];
    for (i, hpath) in hits.iter().enumerate() {
        lines.push(format!("{}. {hpath}", i + 1));
    }
    lines
}

/// Маппинг фетча доков в строки секции: отказ ядра — статус внутри
/// секции, команда не падает (дисциплина semantic-секции).
pub(super) fn doc_section_result_lines(
    query: &str,
    limit: usize,
    fetch: Result<Vec<String>>,
) -> Vec<String> {
    match fetch {
        Ok(hpaths) => doc_section_lines(query, &hpaths, limit),
        Err(e) => vec![
            DOC_SECTION_HEADER.to_string(),
            format!("kernel sql недоступен: {e:#}"),
        ],
    }
}

/// Сборка stdout search между головой и SOFA/семантикой: полнотекст,
/// секция доков. Порядок и форматы — контракт парсинга фабричных агентов.
/// Голову печатает search_run ДО вызова — отказ полнотекстового фетча
/// (`?` здесь) выходит, когда голова уже в stdout (семантика мастера).
/// Публичный шов e2e (`cli/tests/search_e2e.rs`) — живое ядро гоняет
/// полный production-путь через него.
pub async fn search_lines(sy: &SiYuan, query: &str, limit: usize) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    lines.extend(fulltext_lines(&sy.search(query, limit).await?));
    let fetch = sy.doc_titles(DOC_TITLES_CAP).await;
    lines.extend(doc_section_result_lines(query, limit, fetch));
    Ok(lines)
}

pub(crate) async fn search_run(
    query: &str,
    limit: usize,
    threshold: f32,
    sofa: usize,
) -> Result<()> {
    let sy = SiYuan::from_env()?;
    // Голова — до полнотекстового фетча: отказный путь stdout байт-в-байт
    // прежний (голова + пустая строка, затем ошибка) — контракт парсинга.
    println!("{}\n", search_head(query));
    for line in search_lines(&sy, query, limit).await? {
        println!("{line}");
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
    let md = sy.export_md_by_id(&doc_id).await?;
    for warning in md_artifacts::artifact_report(&md) {
        eprintln!("⚠ round-trip-артефакт: {warning}");
    }
    Ok(md)
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

/// Параметры rewrite-пути task body (replace / replace-section).
struct BodyRewrite<'a> {
    /// Секционная правка: existing-база сохраняется механикой replace_md_section,
    /// гард свежести не несёт; None — полный replace целиком.
    section: Option<&'a str>,
    /// Снапшот свежести --base (текст файла); None + !force = отказ.
    snapshot: Option<&'a str>,
    /// Осознанный overwrite без сверки.
    force: bool,
}

/// Запись тела in-place: docId сохраняется, привязка строки не рвётся.
/// rewrite=None — append: block-хирургия именованных html_embed-фенсов
/// (update/insert/delete по маркерам) + один appendBlock остатка; Some —
/// экспорт по id, strip frontmatter (иначе он ляжет литеральным блоком),
/// фенсы инлайнятся парами «маркер + html», пересборка контента.
/// Отказ гарда — Err до overwrite: экспорт read-only, ядро не тронуто.
/// Валидация ввода (дубль имени, пустая строка в html) — Err до любых мутаций.
async fn write_body_in_place(
    sy: &SiYuan,
    doc_id: &str,
    doc_title: &str,
    rewrite: Option<BodyRewrite<'_>>,
    content: &str,
) -> Result<()> {
    let Some(rewrite) = rewrite else {
        return append_with_diagrams(sy, doc_id, content).await;
    };
    let inline = html_embed::parse_input(content)?.inline_md();
    // титульный H1 из контента (снапшот-пасты) — дубль к док-заголовку:
    // стрип до сборки и записи, инвариант «не более одного H1 = док-тайтл»
    let inline = markdown::strip_title_h1(&inline, doc_title);
    let existing_raw = sy.export_md_by_id(doc_id).await.unwrap_or_default();
    let existing = markdown::strip_export_wrapper(&existing_raw, doc_title);
    let new_md = match rewrite.section {
        Some(section) => {
            let (md, edit) = replace_md_section(&existing, section, &inline)?;
            report_section_edit(section, edit);
            md
        }
        None => {
            if !rewrite.force {
                require_fresh_snapshot(doc_title, &existing, rewrite.snapshot)?;
            }
            carry_over_comments_section(doc_title, &existing, &inline)?
        }
    };
    sy.overwrite_doc_md(doc_id, doc_title, &new_md).await
}

/// append-путь с фенсами: именованные — block-хирургия в доке (адресация
/// по маркер-абзацам из .sy-порядка детей), потом остаток одним appendBlock.
/// Живые html-блоки вне фенсов не трогаются (экспорт ядра отдаёт их сырым
/// HTML, append остатка их не пересобирает).
async fn append_with_diagrams(sy: &SiYuan, doc_id: &str, content: &str) -> Result<()> {
    let parsed = html_embed::parse_input(content)?;
    for fence in parsed.named_fences() {
        let children = sy.doc_children(doc_id).await?;
        let location =
            sy_find_marker_location(&children, html_embed::DIAGRAM_MARKER_PREFIX, &fence.name);
        let op = html_embed::plan_append_op(location.as_ref(), &fence);
        apply_diagram_op(sy, doc_id, op).await?;
    }
    let residual = parsed.residual_md();
    if !residual.trim().is_empty() {
        sy.append_block_md(doc_id, &residual).await?;
    }
    Ok(())
}

/// Системная секция «Комменты» переживает полный replace: если новый контент
/// её не несёт, существующая переносится в конец дословно (история диалога
/// юзер ↔ агент невосстановима — инцидент 06.11, крон «stable-ватчер»).
/// Контент с h2-секцией пишется как есть — осознанная замена; осознанный снос
/// — пустая секция в контенте. Тёзка «Комменты» на другом уровне — отказ:
/// перенос создал бы дубль (гард по образцу replace_md_section, ADR-048).
/// Инвариант системной секции, не сверка: действует и при --force.
fn carry_over_comments_section(
    doc_title: &str,
    existing: &str,
    content: &str,
) -> anyhow::Result<String> {
    if crate::md_heading::has_h2_section(content, "Комменты") {
        return Ok(content.to_string());
    }
    if let Some((level, twin_line)) =
        crate::md_heading::heading_twin_outside_h2(content, "Комменты")
    {
        bail!(
            "контент несёт «Комменты» на уровне h{level}: «{twin_line}» — авто-перенос системной \
             секции создал бы дубль. Включи секцию «## Комменты» в контент осознанно (замена/снос \
             истории) или переименуй подсекцию"
        );
    }
    let Some(comments) = crate::md_heading::extract_h2_section(existing, "Комменты") else {
        return Ok(content.to_string());
    };
    let mut carried = content.trim_end().to_string();
    if carried.is_empty() {
        bail!("пустой контент replace тела «{doc_title}» — используй append без --replace");
    }
    carried.push_str("\n\n");
    carried.push_str(comments.trim_end());
    carried.push('\n');
    refuse_fragmented_comment_tail(doc_title, existing, &carried)?;
    Ok(carried)
}

/// Хвостовой гард фрагментации «Комменты» (инцидент 08.11, 20261108044740):
/// чужой h2 после секции (replace-section «не найдена — в конец») закрывает
/// её, поздние комменты (task comment аппендит в конец дока) остаются за
/// пределами — авто-перенос берёт только головной фрагмент, хвост пропадает
/// молча. Свежесть --base тут бессильна: снапшот несёт ту же фрагментацию,
/// нормализованная сверка сходится. Комменты хвоста, отсутствующие в
/// carried (контент + перенесённая секция), — отказ с перечнем, пока потеря
/// поправима; влитые агентом в контент матчатся подстрокой — гард молчит.
/// Осознанная замена истории (контент с собственной «## Комменты») сюда
/// не доходит — ветка выше.
fn refuse_fragmented_comment_tail(
    doc_title: &str,
    existing: &str,
    carried: &str,
) -> anyhow::Result<()> {
    let Some(tail) = crate::md_heading::tail_after_h2_section(existing, "Комменты") else {
        return Ok(());
    };
    let lost: Vec<String> = syt::comment_lines(&tail)
        .into_iter()
        .filter(|line| !carried.contains(line.as_str()))
        .collect();
    if lost.is_empty() {
        return Ok(());
    }
    let listed: String = lost.iter().map(|line| format!("  {line}\n")).collect();
    bail!(
        "секция «Комменты» в теле «{doc_title}» фрагментирована чужим h2: за её пределами сидят \
         хвостовые комменты — полный replace переносит только головную секцию, хвост пропал бы \
         молча:\n{listed}Влей перечисленные комменты в контент (в секцию ## Комменты нового тела) \
         и повтори; осознанный снос хвоста — включи в контент собственную секцию ## Комменты \
         (замена истории)"
    );
}

/// Freshness-гард полного --replace (потеря апдейта — класс инцидента 05.11
/// «Стек и архитектура», путь task body тот же класс: инцидент 06.11, крон
/// «stable-ватчер»). Снапшот --base — нетронутый вывод task get (или сырой
/// экспорт/чистое тело), снятый до правки: сверка со свежим экспортом
/// нормализованная (normalized_lines гасит рендер-шум round-trip — ложный
/// отказ на whitespace-дрейфе невозможен). Без снапшота сверять не с чем —
/// отказ с рецептом, не тихая запись. Существующая секция «Комменты» — часть
/// тела: свой коммент (task comment) между снапшотом и replace — легитимная
/// правка, гард честно откажет; порядок — replace → comment.
fn require_fresh_snapshot(doc_title: &str, existing: &str, snapshot: Option<&str>) -> Result<()> {
    let Some(snapshot_text) = snapshot else {
        bail!(
            "replace требует --base <снапшот.md> — вывод `task get <id>` (допустим сырой \
             экспорт или чистое тело), снятый до правки: workflow «task get > снапшот → \
             правь → --replace --base снапшот»; без снапшота параллельная правка тела \
             теряется тихо. --force — осознанный overwrite без сверки"
        );
    };
    let snapshot_body =
        markdown::strip_export_wrapper(&strip_task_get_output(snapshot_text), doc_title);
    if markdown::normalized_lines(&snapshot_body) != markdown::normalized_lines(existing) {
        bail!(
            "тело менялось после снапшота (--base) — replace затёр бы чужие правки. \
             Перечитай текущий контент: task get <id>; влей дельту в новый контент, \
             пересними снапшот и повтори; --force — overwrite без сверки. Если между \
             снапшотом и replace тело комментировалось (task comment) — это тоже \
             правка: комменты после replace"
        );
    }
    Ok(())
}

async fn apply_diagram_op(sy: &SiYuan, doc_id: &str, op: html_embed::DiagramOp) -> Result<()> {
    match op {
        html_embed::DiagramOp::UpdateExisting { block_id, html } => {
            sy.update_block_md(&block_id, &html).await
        }
        html_embed::DiagramOp::DeleteExisting { block_id } => sy.delete_block(&block_id).await,
        html_embed::DiagramOp::InsertAfterMarker { marker_id, html } => {
            sy.insert_block_md_after(&marker_id, &html).await
        }
        html_embed::DiagramOp::AppendNewPair { name, html } => {
            // маркер + html одним markdown-вызовом: ядро создаёт оба блока
            // (пара подтверждена пробами ядра 3.8.10)
            let pair = format!("{}\n\n{html}", html_embed::marker_text(&name));
            sy.append_block_md(doc_id, &pair).await
        }
        html_embed::DiagramOp::Nothing => Ok(()),
    }
}

/// Разметка вывода `task get` вокруг тела — не контент: шапка карточки
/// (Задача:/Статус:/…) до строки-маркера «----- тело -----» и хвост «Аттачи:»
/// с путями ассетов после тела. Полный get-вывод, сырой экспорт и чистое
/// тело сводятся к телу (контракт --base). Легитимные строки-маркеры в
/// чистом теле срежут часть контента — false-mismatch, отказ (safe-направление).
/// Маркер — общая константа с write-side детектором (md_artifacts):
/// срезка снапшота и детекция входа не разъедутся.
///
/// Построчный точный матч (задача 20261108045651, по образцу
/// task_get_wrapper_signature): граница — строка, trim_end() которой ровно
/// маркер, вне код-фенса. Матч по ПОДСТРОКЕ ломал срезку на легитимных
/// тайтлах с маркером внутри (строка шапки «Задача:   … «----- тело -----»…»
/// матчилась раньше настоящего маркера — остаток мусорный, сверка --base
/// ложно отказывала). Хвост «Аттачи:» — тем же построчным принципом
/// (префикс-старт строки, паритет с прежним "\nАттачи:"), фенсы не граница.
fn strip_task_get_output(raw: &str) -> String {
    let mut body_start: Option<usize> = None;
    let mut attach_start: Option<usize> = None;
    let mut in_fence = false;
    let mut offset = 0usize;
    for line in raw.lines() {
        let line_end = offset + line.len();
        // lines() режет CRLF-пару: реальный шаг до следующей строки — 2 байта
        // на "\r\n", иначе 1 ("\n" или конец входа)
        let step = if raw[line_end..].starts_with("\r\n") {
            2
        } else {
            1
        };
        if md_heading::toggles_fence(line) {
            in_fence = !in_fence;
        } else if !in_fence {
            if body_start.is_none() && line.trim_end() == md_artifacts::TASK_GET_BODY_MARKER {
                body_start = Some(line_end + step);
            } else if line.starts_with("Аттачи:") && offset > 0 {
                // срез с ведущим "\n" — паритет прежнего матча "\nАттачи:"
                attach_start = Some(offset - 1);
                break;
            }
        }
        offset = line_end + step;
    }
    let start = body_start.unwrap_or(0);
    let end = attach_start.unwrap_or(raw.len());
    // срез дословный (байтовые границы между строками): трейлинг-переводы
    // и CRLF-хвосты строк сохраняются, как при прежнем split_once
    let body = if start >= end { "" } else { &raw[start..end] };
    // ведущие переводы после маркера срезаются: strip_frontmatter дальше
    // требует строгий префикс «---\n» — "\n\n---\n…" свёл бы сверку вразнос
    body.trim_start_matches(['\r', '\n']).to_string()
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
    "create", "list", "get", "edit", "claim", "comment", "thread", "close", "body", "rm", "clean",
    "undo", "set", "pause", "resume",
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
    use super::carry_over_comments_section;
    use super::collect_asset_refs;
    use super::collect_threads;
    use super::replace_md_section;
    use super::require_fresh_snapshot;
    use super::save_body_assets;
    use super::search_head;
    use super::strip_task_get_output;
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
        render_rows_titled(with_binding, "таск")
    }

    fn render_rows_titled(with_binding: bool, title: &str) -> serde_json::Value {
        let mut block = serde_json::json!({ "content": title, "refSubtype": "d" });
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

    /// Ответ MCP render с тайтлом задачи, отличным от дефолтного «таск».
    fn mcp_render_response_titled(with_binding: bool, title: &str) -> String {
        let mut view = render_rows_titled(with_binding, title);
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
        mock_siyuan_with_export_titled(title_doc_ids, with_binding, export_content, "таск").await
    }

    /// Вариант с тайтлом задачи, отличным от дефолтного «таск» (регресс
    /// маркер-подстроки в шапке get-вывода, задача 20261108003111).
    async fn mock_siyuan_with_export_titled(
        title_doc_ids: Vec<String>,
        with_binding: bool,
        export_content: String,
        title: &str,
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
        let task_hpath = format!("/uwuwu/Задачи uwuwu/{title}");
        let export_hpath = task_hpath.clone();
        let block_hpath = task_hpath.clone();
        let render_title = title.to_string();
        Mock::given(method("POST"))
            .and(path("/api/filetree/getIDsByHPath"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let hpath = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
                let data = if hpath == "/uwuwu/Задачи uwuwu" {
                    serde_json::json!([BOARD_DOC])
                } else if hpath == task_hpath {
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
                    serde_json::json!({ "code": 0, "data": { "hPath": export_hpath, "content": export_content } })
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
                                r#"{{"jsonrpc":"2.0","id":2,"result":{{"content":[{{"type":"text","text":"ID: {BOUND_DOC}\nType: NodeDocument\nHPath: {block_hpath}"}}]}}}}"#
                            ),
                        )
                    } else if body.contains("\"block\"") && body.contains("get_children") {
                        // overwrite: док пуст → doc_content_is_empty → append
                        ResponseTemplate::new(200).set_body_string(
                            r#"{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":""}]}}"#,
                        )
                    } else {
                        ResponseTemplate::new(200)
                            .set_body_string(mcp_render_response_titled(with_binding, &render_title))
                    }
                } else {
                    ResponseTemplate::new(200).set_body_string(r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"unexpected"}}"#)
                }
            })
            .mount(&server)
            .await;

        (server, create_calls, append_bodies, delete_calls)
    }

    /// task body --replace (force: false) через task_run: контент пишется в
    /// temp-файл, снапшот --base опционален. Result — для unwrap_err на
    /// тестах отказного пути.
    async fn run_body_replace(
        id: &str,
        snapshot: Option<&std::path::Path>,
        content: &str,
    ) -> anyhow::Result<()> {
        let md = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(md.path(), content).unwrap();
        task_run(
            "uwuwu",
            TaskSub::Body {
                id: id.into(),
                stdin: false,
                md: Some(md.path().to_string_lossy().into_owned()),
                replace: true,
                replace_section: None,
                base: snapshot.map(|p| p.to_string_lossy().into_owned()),
                force: false,
                attach: Vec::new(),
            },
        )
        .await
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
                base: None,
                force: false,
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
                base: None,
                force: false,
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
                base: None,
                force: false,
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

    /// Гард свежести полного --replace: без --base и без --force — отказ с
    /// рецептом до первой мутации (экспорт read-only): ни appendBlock
    /// пересборки, ни deleteBlock-шторма — ядро не тронуто.
    #[tokio::test]
    #[serial_test::serial]
    async fn full_replace_without_snapshot_or_force_refuses_without_kernel_write() {
        let export = "---\ntitle: таск\n---\n\n# таск\n\nтело\n".to_string();
        let (server, create_calls, append_bodies, delete_calls) =
            mock_siyuan_with_export(vec![BOUND_DOC.to_string()], true, export).await;
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
        let err = run_body_replace("таск", None, "новое тело\n")
            .await
            .unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("--base") && msg.contains("--force"),
            "рецепт в отказе: {msg}"
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
        assert_eq!(delete_calls.load(Ordering::SeqCst), 0, "ядро не тронуто");
    }

    /// Свежий снапшот (--base) — полный вывод task get (шапка карточки +
    /// обёртка экспорта + рендер-шум пустыми строками + хвост «Аттачи:») —
    /// сверка проходит нормализованно и запись состоялась.
    #[tokio::test]
    #[serial_test::serial]
    async fn full_replace_with_fresh_task_get_snapshot_writes() {
        let export =
            "---\ntitle: таск\n---\n\n# таск\n\nтело\n\n## Комменты\n- коммент\n".to_string();
        let (server, create_calls, append_bodies, _deletes) =
            mock_siyuan_with_export(vec![BOUND_DOC.to_string()], true, export).await;
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
        // снапшот — «task get целиком»: рендер-шум (пустые строки) не рвёт сверку
        let snapshot = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            snapshot.path(),
            "Задача:   таск\nСтатус:   Design\nСтрока:   item-1\n\n----- тело -----\n\n---\ntitle: таск\n---\n\n# таск\n\nтело\n\n\n## Комменты\n\n\n- коммент\n\nАттачи:\n  /tmp/uwuwu-task-x/скрин.png\n",
        )
        .unwrap();
        run_body_replace("таск", Some(snapshot.path()), "новое тело\n")
            .await
            .unwrap();

        assert_eq!(create_calls.load(Ordering::SeqCst), 0);
        {
            let appends = append_bodies.lock().unwrap();
            assert!(
                appends.iter().any(|p| p.contains("новое тело")),
                "запись состоялась: {:?}",
                *appends
            );
        }
    }

    /// Свежий снапшот на тайтле с маркер-подстрокой (живой кейс
    /// 20261108003111): срезка --base по ровной строке маркера — replace
    /// пишет, ложно отказа «тело менялось» нет.
    #[tokio::test]
    #[serial_test::serial]
    async fn full_replace_with_fresh_snapshot_with_marker_title_writes() {
        let title = "тайтл с «----- тело -----» внутри";
        let export = format!("---\ntitle: {title}\n---\n\n# {title}\n\nтело\n");
        let (server, create_calls, append_bodies, _deletes) =
            mock_siyuan_with_export_titled(vec![BOUND_DOC.to_string()], true, export, title).await;
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
        let snapshot = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            snapshot.path(),
            format!(
                "Задача:   {title}\nСтатус:   Design\nСтрока:   item-1\n\n----- тело -----\n\n---\ntitle: {title}\n---\n\n# {title}\n\nтело\n\nАттачи:\n  /tmp/uwuwu-task-x/скрин.png\n"
            ),
        )
        .unwrap();
        run_body_replace(title, Some(snapshot.path()), "новое тело\n")
            .await
            .unwrap();
        assert_eq!(create_calls.load(Ordering::SeqCst), 0);
        {
            let appends = append_bodies.lock().unwrap();
            assert!(
                appends.iter().any(|p| p.contains("новое тело")),
                "запись состоялась: {:?}",
                *appends
            );
        }
    }

    /// Устаревший снапшот (тело менялось после снятия) — отказ «менялся
    /// после снапшота» до первой мутации: ядро не тронуто.
    #[tokio::test]
    #[serial_test::serial]
    async fn full_replace_with_stale_snapshot_refuses_without_kernel_write() {
        let export = "---\ntitle: таск\n---\n\n# таск\n\nтело\n\nпараллельная правка\n".to_string();
        let (server, _create, append_bodies, delete_calls) =
            mock_siyuan_with_export(vec![BOUND_DOC.to_string()], true, export).await;
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
        let snapshot = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(snapshot.path(), "----- тело -----\n\nтело\n").unwrap();
        let err = run_body_replace("таск", Some(snapshot.path()), "новое тело\n")
            .await
            .unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("меняло")
                && msg.contains("после снапшота")
                && msg.contains("task comment"),
            "суть отказа и порядок replace → comment: {msg}"
        );
        {
            let appends = append_bodies.lock().unwrap();
            assert!(appends.is_empty(), "записи нет: {:?}", *appends);
        }
        assert_eq!(delete_calls.load(Ordering::SeqCst), 0, "ядро не тронуто");
    }

    /// --force старше --base: осознанный overwrite идёт без сверки —
    /// устаревший снапшот не мешает записи.
    #[tokio::test]
    #[serial_test::serial]
    async fn full_replace_with_force_writes_despite_stale_snapshot() {
        let export = "---\ntitle: таск\n---\n\n# таск\n\nтело\n\nпараллельная правка\n".to_string();
        let (server, _create, append_bodies, _deletes) =
            mock_siyuan_with_export(vec![BOUND_DOC.to_string()], true, export).await;
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
        let snapshot = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(snapshot.path(), "----- тело -----\n\nтело\n").unwrap();
        let md = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(md.path(), "новое тело\n").unwrap();
        task_run(
            "uwuwu",
            TaskSub::Body {
                id: "таск".into(),
                stdin: false,
                md: Some(md.path().to_string_lossy().into_owned()),
                replace: true,
                replace_section: None,
                base: Some(snapshot.path().to_string_lossy().into_owned()),
                force: true,
                attach: Vec::new(),
            },
        )
        .await
        .unwrap();

        {
            let appends = append_bodies.lock().unwrap();
            assert!(
                appends.iter().any(|p| p.contains("новое тело")),
                "force-запись состоялась: {:?}",
                *appends
            );
        }
    }

    /// Авто-перенос системной секции на живом мок-ядре: existing с комментами,
    /// контент без «## Комменты» → запись несёт контент + перенесённую секцию.
    #[tokio::test]
    #[serial_test::serial]
    async fn full_replace_carries_over_comments_section_into_write() {
        let export =
            "---\ntitle: таск\n---\n\n# таск\n\nтело\n\n## Комменты\n- **AGENTS** (ts): тик\n"
                .to_string();
        let (server, _create, append_bodies, _deletes) =
            mock_siyuan_with_export(vec![BOUND_DOC.to_string()], true, export).await;
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
        let snapshot = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            snapshot.path(),
            "----- тело -----\n\n---\ntitle: таск\n---\n\n# таск\n\nтело\n\n## Комменты\n- **AGENTS** (ts): тик\n",
        )
        .unwrap();
        run_body_replace("таск", Some(snapshot.path()), "переписанное тело\n")
            .await
            .unwrap();

        {
            let appends = append_bodies.lock().unwrap();
            let payload = appends.join("\n");
            assert!(
                payload.contains("переписанное тело"),
                "контент записан: {payload}"
            );
            assert!(
                payload.contains("## Комменты") && payload.contains("- **AGENTS** (ts): тик"),
                "история комментов пережила replace: {payload}"
            );
        }
    }

    #[test]
    fn strip_task_get_output_cuts_card_header_and_attach_tail() {
        let full_get = "Задача:   таск\nСтатус:   Design\nСтрока:   item-1\n\n----- тело -----\n\n---\ntitle: таск\n---\n\nтело\n\nАттачи:\n  /tmp/uwuwu-task-x/скрин.png\n";
        assert_eq!(
            strip_task_get_output(full_get),
            "---\ntitle: таск\n---\n\nтело\n"
        );
        let raw_export = "---\ntitle: таск\n---\n\nтело\n";
        assert_eq!(strip_task_get_output(raw_export), raw_export);
        let clean_body = "тело без разметки\n";
        assert_eq!(strip_task_get_output(clean_body), clean_body);
    }

    /// Регресс 20261108003111 (BDD-сценарий задачи 20261108045651): тайтл с
    /// маркер-подстрокой попадает в шапку get-вывода раньше настоящего
    /// маркера — срезка по ПОДСТРОКЕ (split_once) даёт мусорный остаток
    /// («…» внутри\nСтатус: …»), сверка --base ложно отказывает. Построчный
    /// матч по ровной строке маркера остаток не портит.
    #[test]
    fn strip_task_get_output_survives_marker_substring_in_header() {
        let full_get = "Задача:   тайтл с «----- тело -----» внутри\nСтатус:   Design\nСтрока:   item-1\n\n----- тело -----\n\n---\ntitle: таск\n---\n\nтело\n";
        let stripped = strip_task_get_output(full_get);
        assert!(
            stripped.starts_with("---\ntitle: таск"),
            "остаток = тело, получено: {stripped:?}"
        );
    }

    /// Ровная строка маркера внутри код-фенса чистого тела — не граница
    /// шапки (зеркало write-side тестов task_get_wrapper_signature): вход
    /// проходит насквозь без срезки.
    #[test]
    fn strip_task_get_output_marker_line_in_fence_is_not_boundary() {
        let clean_body = "текст\n\n```gherkin\n----- тело -----\n```\n\nхвост\n";
        assert_eq!(strip_task_get_output(clean_body), clean_body);
    }

    /// Строка «Аттачи:» внутри код-фенса тела — не граница хвоста: срезка —
    /// по настоящему «Аттачи:» вне фенса.
    #[test]
    fn strip_task_get_output_attach_line_in_fence_is_not_tail_boundary() {
        let full_get = "Задача:   таск\nСтатус:   Design\n\n----- тело -----\n\nшапка тела\n\n```\nАттачи:\n```\n\nхвост тела\n\nАттачи:\n  /tmp/uwuwu-task-x/скрин.png\n";
        let stripped = strip_task_get_output(full_get);
        assert!(
            stripped.contains("```\nАттачи:\n```"),
            "фенс-«Аттачи:» не граница: {stripped:?}"
        );
        assert!(
            stripped.ends_with("хвост тела\n"),
            "срезано по настоящему хвосту: {stripped:?}"
        );
    }

    /// CRLF-вход: lines()+trim_end гасит \r — маркер и «Аттачи:» матчатся,
    /// ведущие пустые строки после маркера срезаются без \r-остатка.
    #[test]
    fn strip_task_get_output_handles_crlf_lines() {
        let full_get = "Задача:   таск\r\nСтатус:   Design\r\n\r\n----- тело -----\r\n\r\nтело\r\n\r\nАттачи:\r\n  /tmp/x.png\r\n";
        let stripped = strip_task_get_output(full_get);
        assert!(
            stripped.starts_with("тело"),
            "остаток после CRLF: {stripped:?}"
        );
        assert!(!stripped.contains("Аттачи:"), "хвост срезан: {stripped:?}");
    }

    #[test]
    fn require_fresh_snapshot_accepts_noise_and_refuses_delta() {
        let existing = "тело\n\n## Комменты\n- коммент\n";
        // свежий снапшот с рендер-шумом: сверка нормализованная, не байтовая
        let noisy = "шапка\n----- тело -----\n\nтело\n\n\n## Комменты\n\n\n- коммент\n";
        require_fresh_snapshot("таск", existing, Some(noisy)).unwrap();
        // чужая правка после снапшота
        let stale = "----- тело -----\n\nтело без секции\n";
        let err = require_fresh_snapshot("таск", existing, Some(stale)).unwrap_err();
        assert!(err.to_string().contains("меняло"));
        assert!(err.to_string().contains("после снапшота"));
        // снапшота нет вовсе
        let err = require_fresh_snapshot("таск", existing, None).unwrap_err();
        assert!(err.to_string().contains("--base") && err.to_string().contains("--force"));
    }

    #[test]
    fn carry_over_comments_appends_existing_section_verbatim() {
        let existing = "старое тело\n\n## Комменты\n- **AGENTS** (2026-11-06):  вердикт ^abc123\n  вложенный ответ\n";
        let out = carry_over_comments_section("таск", existing, "новое тело\n").unwrap();
        assert!(out.starts_with("новое тело\n"), "контент основа: {out}");
        assert!(out.contains(
            "## Комменты\n- **AGENTS** (2026-11-06):  вердикт ^abc123\n  вложенный ответ"
        ));
    }

    #[test]
    fn carry_over_comments_keeps_content_with_its_own_section() {
        let existing = "старое\n\n## Комменты\n- старый коммент\n";
        let out = carry_over_comments_section(
            "таск",
            existing,
            "новое\n\n## Комменты\n- новая история\n",
        )
        .unwrap();
        assert_eq!(out, "новое\n\n## Комменты\n- новая история\n");
    }

    #[test]
    fn carry_over_comments_refuses_twin_on_other_level() {
        let existing = "старое\n\n## Комменты\n- коммент\n";
        let err =
            carry_over_comments_section("таск", existing, "новое\n\n### Комменты\n").unwrap_err();
        assert!(err.to_string().contains("h3") && err.to_string().contains("дубль"));
    }

    #[test]
    fn carry_over_comments_noop_without_existing_section() {
        let out = carry_over_comments_section("таск", "старое без секции\n", "новое\n").unwrap();
        assert_eq!(out, "новое\n");
    }

    /// Фикстура инцидента 08.11 (20261108044740): qlty-секция вставилась после
    /// «Комменты» (replace-section «не найдена — в конец»), поздние комменты
    /// легли за чужим h2 (task comment аппендит в конец дока).
    fn fragmented_existing() -> String {
        "\
старое тело

## Комменты
- **AGENTS** (2026-11-08 10:00): головной коммент

## Машинный ревью (qlty)
находки линтера

- **AGENTS** (2026-11-08 11:00): PR готов к мержу
- **Юра** (2026-11-08 11:30): мержу сам
"
        .to_string()
    }

    #[test]
    fn carry_over_comments_refuses_fragmented_section_tail_loss() {
        let err = carry_over_comments_section("таск", &fragmented_existing(), "новое тело\n")
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("фрагментирована"), "суть ошибки: {msg}");
        assert!(
            msg.contains("PR готов к мержу") && msg.contains("мержу сам"),
            "перечень хвостовых комментов: {msg}"
        );
        assert!(
            !msg.contains("головной коммент"),
            "головной коммент переносится, в потерях не числится: {msg}"
        );
    }

    #[test]
    fn carry_over_comments_passes_when_tail_comments_inlined_into_content() {
        let content = "\
новое тело

## Машинный ревью (qlty)
находки линтера

- **AGENTS** (2026-11-08 11:00): PR готов к мержу
- **Юра** (2026-11-08 11:30): мержу сам
";
        let out = carry_over_comments_section("таск", &fragmented_existing(), content).unwrap();
        assert!(out.contains("PR готов к мержу"), "хвост сохранён: {out}");
        assert!(
            out.contains("## Комменты\n- **AGENTS** (2026-11-08 10:00): головной коммент"),
            "головная секция перенесена: {out}"
        );
    }

    #[test]
    fn carry_over_comments_allows_conscious_replacement_despite_fragmentation() {
        let out = carry_over_comments_section(
            "таск",
            &fragmented_existing(),
            "новое\n\n## Комменты\n- новая история\n",
        )
        .unwrap();
        assert_eq!(out, "новое\n\n## Комменты\n- новая история\n");
    }

    #[test]
    fn carry_over_comments_tolerates_foreign_tail_without_comment_lines() {
        let existing = "старое\n\n## Комменты\n- **AGENTS** (2026-11-08 10:00): головной\n\n## Машинный ревью (qlty)\nнаходки линтера\n";
        let out = carry_over_comments_section("таск", existing, "новое\n").unwrap();
        assert!(out.contains("головной"), "секция перенесена: {out}");
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

    /// Контроль: чистый позиционный текст при пустом --project — гвард
    /// пропускает, команда уходит в глобальный резолв (bail «--project
    /// обязателен» для id-семейства снят задачей 20261106230303; промах
    /// резолва честный, не «флаг не передан»).
    #[tokio::test]
    async fn clean_comment_text_without_project_goes_to_global_resolve() {
        let err = task_run("", comment_command(false, &["текст"]))
            .await
            .expect_err("глобальный резолв без ядра/досок — отказ");
        let msg = err.to_string();
        assert!(
            !msg.contains("похож на флаг") && !msg.contains("подкоманд"),
            "чистый текст гвардом не отклоняется: {msg}"
        );
        assert!(
            !msg.contains("обязателен"),
            "bail «--project обязателен» для comment снят: {msg}"
        );
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
            task_type: String::new(),
            branch: String::new(),
            pr: String::new(),
            dev_session: String::new(),
            planned: String::new(),
            schedule_cell: String::new(),
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
        Arc<Mutex<Vec<(String, String)>>>,
    ) {
        let server = MockServer::start().await;
        let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let appends: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let attrs: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
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
                                "data": { "keys": [
                                    { "id": TITLE_KEY, "name": "Status" },
                                    { "id": "key-type", "name": "Тип" }
                                ] } }).to_string()
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

        Mock::given(method("POST"))
            .and(path("/api/attr/setBlockAttrs"))
            .respond_with({
                let attrs = attrs.clone();
                move |req: &wiremock::Request| {
                    let body: serde_json::Value =
                        serde_json::from_slice(&req.body).unwrap_or_default();
                    if let Some((name, value)) = body
                        .get("attrs")
                        .and_then(|a| a.as_object())
                        .and_then(|a| a.iter().next())
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    {
                        attrs.lock().unwrap().push((name, value));
                    }
                    ResponseTemplate::new(200).set_body_string(r#"{"code":0}"#)
                }
            })
            .mount(&server)
            .await;

        (server, order, appends, attrs)
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
                schedule: None,
                agent: None,
                dry_run: false,
            },
        )
        .await
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn create_with_attach_uploads_before_creating_and_appends_refs() {
        let (server, order, appends, _attrs) = mock_siyuan_for_create(None).await;
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
            vec!["upload", "upload", "create", "item_add", "append"],
            "порядок: upload все → createDoc → строка → референсы (тип — клетка доски, не append)"
        );
        let appends = appends.lock().unwrap();
        let refs_md: String = serde_json::from_str::<serde_json::Value>(&appends[0])
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

    /// create --agent: attr custom-agent записан на теле свежей строки
    /// (свойство документа, клетки на доске нет).
    #[tokio::test]
    #[serial_test::serial]
    async fn create_with_agent_writes_custom_attr() {
        let (server, _order, _appends, attrs) = mock_siyuan_for_create(None).await;

        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
        task_run(
            "uwuwu",
            TaskSub::Create {
                title: "аудитор проекта".into(),
                r#type: None,
                stdin: false,
                attach: Vec::new(),
                schedule: None,
                agent: Some("code-quality-reviewer".into()),
                dry_run: false,
            },
        )
        .await
        .expect("create --agent ok");

        let writes = attrs.lock().unwrap().clone();
        assert_eq!(
            writes,
            vec![(
                "custom-agent".to_string(),
                "code-quality-reviewer".to_string()
            )],
            "ровно одна запись атрибута на теле: {writes:?}"
        );
    }

    /// create --agent "мусор": slug-грамматика отвергает ДО мутаций
    /// (дока нет, attr-записей нет).
    #[tokio::test]
    #[serial_test::serial]
    async fn create_with_bad_agent_rejected_before_write() {
        let (server, _order, _appends, attrs) = mock_siyuan_for_create(None).await;

        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
        let err = task_run(
            "uwuwu",
            TaskSub::Create {
                title: "аудитор проекта".into(),
                r#type: None,
                stdin: false,
                attach: Vec::new(),
                schedule: None,
                agent: Some("Not A Slug".into()),
                dry_run: false,
            },
        )
        .await
        .expect_err("кривой slug — отказ");

        assert!(err.to_string().contains("slug дефа"), "суть отказа: {err}");
        assert!(
            attrs.lock().unwrap().is_empty(),
            "записей нет: валидация до мутаций"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn create_with_attach_bails_on_duplicate_title_before_any_upload() {
        let (server, order, _appends, _attrs) = mock_siyuan_for_create(Some("новая задача")).await;
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
                base: None,
                force: false,
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

    // ——— html_embed: append-хирургия и rewrite-инлайн ———

    const MARKER_ARCH: &str = "⁂ diagram:arch";
    const MARKER_BLOCK: &str = "20261105120100-marker1";
    const HTML_BLOCK: &str = "20261105120200-htmldoc1";

    /// .sy-док с парой «маркер arch + живой html-блок» (формат ядра 3.8.10).
    fn sy_doc(children: &[serde_json::Value]) -> String {
        serde_json::json!({ "ID": BOUND_DOC, "Children": children }).to_string()
    }

    fn marker_child() -> serde_json::Value {
        serde_json::json!({
            "ID": MARKER_BLOCK, "Type": "NodeParagraph",
            "Children": [{ "Type": "NodeText", "Data": MARKER_ARCH }]
        })
    }

    fn html_child(html: &str) -> serde_json::Value {
        serde_json::json!({ "ID": HTML_BLOCK, "Type": "NodeHTMLBlock", "Data": html })
    }

    fn paragraph_child(id: &str, text: &str) -> serde_json::Value {
        serde_json::json!({
            "ID": id, "Type": "NodeParagraph",
            "Children": [{ "Type": "NodeText", "Data": text }]
        })
    }

    /// Моки адресации .sy (getBlockInfo + getFile) поверх базового стенда:
    /// при необходимости путь (значение Children массива) переключается
    /// ответы по порядку вызовов.
    async fn mount_doc_sy(server: &MockServer, sy_docs: Vec<String>) {
        let docs = std::sync::Arc::new(std::sync::Mutex::new(sy_docs));
        Mock::given(method("POST"))
            .and(path("/api/block/getBlockInfo"))
            .respond_with(move |_req: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_string(
                    serde_json::json!({
                        "code": 0,
                        "data": { "box": "box-1", "path": "/20261105120000-boundd1.sy" }
                    })
                    .to_string(),
                )
            })
            .mount(server)
            .await;
        let queue = docs.clone();
        Mock::given(method("POST"))
            .and(path("/api/file/getFile"))
            .respond_with(move |_req: &wiremock::Request| {
                let mut queue = queue.lock().unwrap();
                let body = if queue.len() == 1 {
                    queue[0].clone()
                } else {
                    queue.remove(0)
                };
                ResponseTemplate::new(200).set_body_string(body)
            })
            .mount(server)
            .await;
    }

    async fn mount_block_api(server: &MockServer) -> (Arc<Mutex<Vec<String>>>, Arc<AtomicUsize>) {
        let update_bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let updates = update_bodies.clone();
        Mock::given(method("POST"))
            .and(path("/api/block/updateBlock"))
            .respond_with(move |req: &wiremock::Request| {
                updates
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&req.body).to_string());
                ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":null}"#)
            })
            .mount(server)
            .await;
        let insert_bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let inserts = insert_bodies.clone();
        Mock::given(method("POST"))
            .and(path("/api/block/insertBlock"))
            .respond_with(move |req: &wiremock::Request| {
                inserts
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&req.body).to_string());
                ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":null}"#)
            })
            .mount(server)
            .await;
        (update_bodies, Arc::new(AtomicUsize::new(0)))
    }

    fn append_payloads(append_bodies: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        append_bodies
            .lock()
            .unwrap()
            .iter()
            .map(|b| {
                serde_json::from_str::<serde_json::Value>(b)
                    .expect("appendBlock body — json")
                    .get("data")
                    .and_then(|d| d.as_str())
                    .expect("data — markdown")
                    .to_string()
            })
            .collect()
    }

    async fn write_md_content(content: &str, replace: bool, section: Option<String>) {
        let md = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(md.path(), content).unwrap();
        task_run(
            "uwuwu",
            TaskSub::Body {
                id: "таск".into(),
                stdin: false,
                md: Some(md.path().to_string_lossy().into_owned()),
                replace,
                replace_section: section,
                // тесты про конвертацию фенсов, не про freshness-гард (#119):
                // force снимает требование снапшота при полном replace
                base: None,
                force: replace,
                attach: Vec::new(),
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn append_named_fence_updates_existing_diagram_in_place() {
        let (server, create_calls, append_bodies, _deletes) =
            mock_siyuan(vec![BOUND_DOC.to_string()], true).await;
        mount_doc_sy(
            &server,
            vec![sy_doc(&[marker_child(), html_child("<div>old</div>")])],
        )
        .await;
        let (update_bodies, _inserts) = mount_block_api(&server).await;
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");

        write_md_content(
            "текст рядом\n```html_embed arch\n<div>new</div>\n```\n",
            false,
            None,
        )
        .await;

        assert_eq!(create_calls.load(Ordering::SeqCst), 0);
        let updates = update_bodies.lock().unwrap();
        assert_eq!(
            updates.len(),
            1,
            "один updateBlock по html-блоку после маркера"
        );
        let payload = serde_json::from_str::<serde_json::Value>(&updates[0]).unwrap();
        assert_eq!(payload.get("id").unwrap(), HTML_BLOCK);
        assert_eq!(payload.get("data").unwrap(), "<div>new</div>");
        drop(updates);
        let appends = append_payloads(&append_bodies);
        assert_eq!(
            appends,
            vec!["текст рядом\n"],
            "residual без маркеров и html"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn append_named_fence_without_marker_appends_marker_pair() {
        let (server, create_calls, append_bodies, _deletes) =
            mock_siyuan(vec![BOUND_DOC.to_string()], true).await;
        mount_doc_sy(
            &server,
            vec![sy_doc(&[paragraph_child("p-1", "обычный текст")])],
        )
        .await;
        let (_update_bodies, _inserts) = mount_block_api(&server).await;
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");

        write_md_content("```html_embed arch\n<div>fresh</div>\n```\n", false, None).await;

        assert_eq!(create_calls.load(Ordering::SeqCst), 0);
        let appends = append_payloads(&append_bodies);
        assert_eq!(appends.len(), 1, "пара маркер+html одним appendBlock");
        assert_eq!(appends[0], "⁂ diagram:arch\n\n<div>fresh</div>");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn append_empty_named_fence_deletes_diagram() {
        let (server, create_calls, append_bodies, delete_calls) =
            mock_siyuan(vec![BOUND_DOC.to_string()], true).await;
        mount_doc_sy(
            &server,
            vec![sy_doc(&[marker_child(), html_child("<div>old</div>")])],
        )
        .await;
        let (_update_bodies, _inserts) = mount_block_api(&server).await;
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");

        write_md_content("```html_embed arch\n```\n", false, None).await;

        assert_eq!(create_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            delete_calls.load(Ordering::SeqCst),
            1,
            "deleteBlock html-блока диаграммы"
        );
        let appends = append_payloads(&append_bodies);
        assert!(appends.is_empty(), "удаление без residual не аппендит");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn append_anonymous_fence_inlines_html_without_doc_reads() {
        let (server, create_calls, append_bodies, _deletes) =
            mock_siyuan(vec![BOUND_DOC.to_string()], true).await;
        let info_hits = Arc::new(AtomicUsize::new(0));
        let hits = info_hits.clone();
        Mock::given(method("POST"))
            .and(path("/api/block/getBlockInfo"))
            .respond_with(move |_req: &wiremock::Request| {
                hits.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":{}}"#)
            })
            .mount(&server)
            .await;
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");

        write_md_content("```html_embed\n<div>anon</div>\n```\n", false, None).await;

        assert_eq!(create_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            info_hits.load(Ordering::SeqCst),
            0,
            "анонимный фенс не адресует док — хирургия не нужна"
        );
        let appends = append_payloads(&append_bodies);
        assert_eq!(
            appends,
            vec!["<div>anon</div>\n\n"],
            "blank-guard после html"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn replace_inlines_named_fence_as_marker_pair() {
        let export = "---\ntitle: таск\n---\n\n# таск\n\nстарое тело\n".to_string();
        let (server, create_calls, append_bodies, _deletes) =
            mock_siyuan_with_export(vec![BOUND_DOC.to_string()], true, export).await;
        let (_update_bodies, _inserts) = mount_block_api(&server).await;
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");

        write_md_content(
            "новое тело\n```html_embed arch\n<div>new</div>\n```\n",
            true,
            None,
        )
        .await;

        assert_eq!(create_calls.load(Ordering::SeqCst), 0);
        let appends = append_payloads(&append_bodies);
        assert_eq!(appends.len(), 1, "overwrite пустого мок-дока — один append");
        assert!(
            appends[0].contains("⁂ diagram:arch\n\n<div>new</div>"),
            "именованный фенс инлайнится парой «маркер + html»: {}",
            appends[0]
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn replace_section_keeps_live_html_outside_section_verbatim() {
        // живой html-блок вне секции: экспорт ядра отдаёт его сырым HTML —
        // перезапись обязана сохранить блок verbatim (High раунда 1)
        let export = "---\ntitle: таск\n---\n\n# таск\n\n<div class=\"live\">diag</div>\n\n## Решение\nстарая секция\n".to_string();
        let (server, create_calls, append_bodies, _deletes) =
            mock_siyuan_with_export(vec![BOUND_DOC.to_string()], true, export).await;
        let (_update_bodies, _inserts) = mount_block_api(&server).await;
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");

        write_md_content(
            "новая секция\n```html_embed arch\n<div>new</div>\n```\n",
            false,
            Some("Решение".into()),
        )
        .await;

        assert_eq!(create_calls.load(Ordering::SeqCst), 0);
        let appends = append_payloads(&append_bodies);
        assert_eq!(appends.len(), 1);
        assert!(
            appends[0].contains("<div class=\"live\">diag</div>"),
            "живой html вне секции переживает перезапись verbatim: {}",
            appends[0]
        );
        assert!(
            appends[0].contains("⁂ diagram:arch"),
            "фенс секции инлайнится парой: {}",
            appends[0]
        );
        assert!(
            !appends[0].contains("```html_embed"),
            "литерального фенса нет"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn create_body_with_named_fence_writes_marker_pair() {
        // мини-стенд create-ветки (тела нет): createDocWithMd capture'ит markdown
        let server = MockServer::start().await;
        let created_md: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let created = created_md.clone();
        Mock::given(method("POST"))
            .and(path("/api/filetree/createDocWithMd"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                created.lock().unwrap().push(
                    body.get("markdown")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .to_string(),
                );
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"code":0,"data":"20261105120300-newdoc01"}"#)
            })
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/query/sql"))
            .respond_with(|req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let stmt = body.get("stmt").and_then(|s| s.as_str()).unwrap_or("");
                // поиск доски (doc_ids_by_hpath идёт через getIDsByHPath; здесь
                // только av-носитель доски) и прочие выборки — пусто
                let data = if stmt.contains("type='av'") {
                    serde_json::json!([{ "id": "block-1", "markdown": "<div data-type=\"NodeAttributeView\" data-av-id=\"av-1\"></div>" }])
                } else {
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
                    r#"{"code":0,"data":{"notebooks":[{"id":"nb-1","name":"Projects"}]}}"#,
                )
            })
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/filetree/getIDsByHPath"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let hpath = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
                let _data = if hpath == "/uwuwu/Задачи uwuwu" {
                    serde_json::json!([BOARD_DOC])
                } else {
                    serde_json::json!([])
                };
                ResponseTemplate::new(200)
                    .set_body_string(serde_json::json!({ "code": 0, "data": _data }).to_string())
            })
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(|req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("\"initialize\"") {
                    ResponseTemplate::new(200)
                        .set_body_string(
                            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":3}}}"#,
                        )
                        .append_header("mcp-session-id", "sess-1")
                } else if body.contains("notifications/initialized") {
                    ResponseTemplate::new(200)
                } else if body.contains("tools/call") {
                    if body.contains("\"block\"") && body.contains("\"get\"") {
                        // create-сценарий: привязки нет и дока нет → doc_block_exists=false
                        ResponseTemplate::new(200).set_body_string(
                            r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"block not found"}}"#,
                        )
                    } else if body.contains("\"block\"") && body.contains("get_children") {
                        ResponseTemplate::new(200).set_body_string(
                            r#"{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":""}]}}"#,
                        )
                    } else {
                        ResponseTemplate::new(200).set_body_string(mcp_render_response(true))
                    }
                } else {
                    ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":null}"#)
                }
            })
            .mount(&server)
            .await;
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");

        write_md_content("```html_embed arch\n<div>first</div>\n```\n", false, None).await;

        let created = created_md.lock().unwrap();
        assert_eq!(created.len(), 1, "тело создано одним createDocWithMd");
        assert!(
            created[0].contains("⁂ diagram:arch\n\n<div>first</div>"),
            "create-ветка пишет пару «маркер + html»: {}",
            created[0]
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn validation_errors_fire_before_network_and_uploads() {
        let (server, create_calls, append_bodies, delete_calls) =
            mock_siyuan(vec![BOUND_DOC.to_string()], true).await;
        let upload_hits = Arc::new(AtomicUsize::new(0));
        let hits = upload_hits.clone();
        Mock::given(method("POST"))
            .and(path("/api/asset/upload"))
            .respond_with(move |_req: &wiremock::Request| {
                hits.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_string(
                    r#"{"code":0,"data":{"succMap":{"a.png":"assets/a-20261106090000-abc1234.png"}}}"#,
                )
            })
            .mount(&server)
            .await;
        std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
        std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");

        let md = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            md.path(),
            "```html_embed arch\n<div>a</div>\n```\n```html_embed arch\n<div>b</div>\n```\n",
        )
        .unwrap();
        let attach = tempfile::NamedTempFile::with_suffix(".png").unwrap();
        std::fs::write(attach.path(), b"png-bytes").unwrap();
        let err = task_run(
            "uwuwu",
            TaskSub::Body {
                id: "таск".into(),
                stdin: false,
                md: Some(md.path().to_string_lossy().into_owned()),
                replace: false,
                replace_section: None,
                base: None,
                force: false,
                attach: vec![attach.path().to_path_buf()],
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("дубль имени"), "{err}");
        assert_eq!(create_calls.load(Ordering::SeqCst), 0, "отказ до сети");
        assert_eq!(
            upload_hits.load(Ordering::SeqCst),
            0,
            "отказ до upload: ассет не загружен"
        );
        assert!(append_payloads(&append_bodies).is_empty());
        assert_eq!(delete_calls.load(Ordering::SeqCst), 0);
    }
}
