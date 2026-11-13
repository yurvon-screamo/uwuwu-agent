//! Hub-ветка task-семьи (UWUWU_HUB_REPO задан): команды через TaskStore
//! (GitHubStore). Перехват в начале task_run — легаси-путь мёртв в
//! hub-режиме. Не-перенесённые подкоманды (rm/clean/undo/goal-семья) —
//! громкий отказ с причиной: коммутация отдельным срезом (задача
//! 20261112155302-ttyp35g, план срез F/B-2).

use crate::cmd::cli::TaskSub;
use crate::store::task_store::{CreateTask, EditTask};
use crate::store::{task_store, TaskBoard, TaskType};
use anyhow::{bail, Result};

/// Гвард «путь-флаг получил не-путь» (ADR-094, процессный контракт).
/// Значение похоже на путь (есть '/') — но файла нет: честная OS-причина
/// (ENOENT caused by) под подсказкой флага. Не-путь (inline-текст) —
/// отказ с усечённым началом значения и stdin-подсказкой. До записей и сети.
fn path_flag(flag: &str, value: &str, stdin_hint: &str) -> Result<()> {
    let looks_like_path = value.contains('/') || value.starts_with('.');
    if !looks_like_path {
        let head: String = value.chars().take(40).collect();
        bail!("{flag} ждёт путь к файлу (получен не-путь: «{head}»); для текста из stdin — {stdin_hint}")
    }
    std::fs::read_to_string(value).map(|_| ()).map_err(|e| {
        anyhow::anyhow!("{flag}: {e}").context(format!(
            "{flag} ждёт путь к файлу; для текста из stdin — {stdin_hint}"
        ))
    })
}

fn make_board(project: &str) -> TaskBoard {
    TaskBoard {
        project: project.to_string(),
    }
}

fn not_migrated(cmd: &str) -> Result<()> {
    bail!(
        "hub-режим: task {cmd} переносится следующим срезом коммутации \
         (легаси-путь вырезан); пока — ручной gh"
    )
}

pub(crate) async fn run(project: &str, command: TaskSub) -> Result<()> {
    let store = task_store(crate::cmd::run::hub_repo_default());
    let board = make_board(project);
    match command {
        TaskSub::List {
            status,
            r#type,
            project: list_project,
        } => {
            let board = match list_project {
                Some(p) => make_board(p.as_str()),
                None => board,
            };
            let mut tasks = store.list(&board).await?;
            if let Some(st) = &status {
                let needle = st.to_lowercase();
                tasks.retain(|t| t.status.to_lowercase().contains(&needle));
            }
            if let Some(ty) = &r#type {
                let needle = ty.to_lowercase();
                tasks.retain(|t| t.task_type.0.to_lowercase() == needle);
            }
            tasks.sort_by_key(|t| t.id.parse::<u64>().unwrap_or(0));
            for t in &tasks {
                let ty = if t.task_type.is_set() {
                    format!(" {:4}", t.task_type.0)
                } else {
                    "    ".to_string()
                };
                println!("{:>8} {:>10}{} {}", t.id, t.status, ty, t.title);
            }
            println!("строк: {}", tasks.len());
            Ok(())
        }
        TaskSub::Get { id, no_body } => {
            let task = store
                .resolve(&board, &id)
                .await
                .map_err(anyhow::Error::new)?;
            println!("Задача:   {}", task.title);
            println!("Статус:   {}", task.status);
            if task.task_type.is_set() {
                println!("Тип:      {}", task.task_type.0);
            }
            if !task.branch.is_empty() {
                println!("Ветка:    {}", task.branch);
            }
            if !task.pr.is_empty() {
                println!("PR:       {}", task.pr);
            }
            if !task.dev_session.is_empty() {
                println!("Сессия:   {}", task.dev_session);
            }
            println!("Строка:   issue #{}", task.id);
            if !no_body {
                let body = store.body_get(&board, &task).await?;
                println!();
                println!("{body}");
            }
            Ok(())
        }
        TaskSub::Create {
            title,
            r#type: ty,
            status,
            stdin,
            attach,
            dry_run,
            priority,
            autodone,
            ..
        } => {
            for path in &attach {
                path_flag("--attach", path.to_str().unwrap_or(""), "--stdin")?;
            }
            if dry_run {
                let body = create_body_md(&title, ty.as_deref().unwrap_or("feature"), "");
                println!("{body}");
                return Ok(());
            }
            if attach.is_empty() {
                let body_md = match stdin {
                    true => Some(std::io::read_to_string(std::io::stdin())?),
                    false => None,
                };
                let created = store
                    .create(
                        &board,
                        &CreateTask {
                            title,
                            task_type: TaskType(ty.unwrap_or_else(|| "feature".into())),
                            status,
                            body_md,
                            attachments: vec![],
                            priority,
                            autodone: autodone.as_deref().map(|v| v == "on"),
                        },
                    )
                    .await?;
                println!(
                    "создана: issue #{} «{}» ({})",
                    created.id, created.title, created.status
                );
                Ok(())
            } else {
                not_migrated("create --attach")
            }
        }
        TaskSub::Edit {
            id,
            status,
            title,
            pr,
            branch,
            r#type,
            priority,
            autodone,
            schedule,
            agent: _,
            planned,
        } => {
            let task = store
                .resolve(&board, &id)
                .await
                .map_err(anyhow::Error::new)?;
            let patch = EditTask {
                status: status.clone(),
                title: title.clone(),
                pr: pr.clone(),
                branch: branch.clone(),
                task_type: r#type.clone().map(TaskType),
                priority: priority.clone(),
                autodone: autodone.as_deref().map(|v| v == "on"),
                planned: planned.as_ref().map(|p| (p.clone(), 0)),
                ..Default::default()
            };
            let fresh = store.edit(&board, &task, &patch).await?;
            if let Some(s) = &schedule {
                // ключ унифицирован: schedule (читают cron list/convert/демоны)
                store.set_body_attr(&board, &fresh, "schedule", s).await?;
            }
            println!("изменена: issue #{} «{}»", fresh.id, fresh.title);
            Ok(())
        }
        TaskSub::Claim { id, session } => {
            let task = store
                .resolve(&board, &id)
                .await
                .map_err(anyhow::Error::new)?;
            let session = session
                .clone()
                .or_else(|| std::env::var("OPENCODE_SESSION_ID").ok())
                .ok_or_else(|| {
                    anyhow::anyhow!("нет sessionID: --session или OPENCODE_SESSION_ID")
                })?;
            let claimed = store.claim(&board, &task, &session).await?;
            println!(
                "заявлена: issue #{} сессия {}",
                claimed.id, claimed.dev_session
            );
            Ok(())
        }
        TaskSub::Comment {
            id,
            reply,
            stdin,
            md,
            text,
        } => {
            if let Some(path) = &md {
                path_flag("--md", path, "--stdin")?;
            }
            let task = store
                .resolve(&board, &id)
                .await
                .map_err(anyhow::Error::new)?;
            let body = match (stdin, &md) {
                (true, _) => std::io::read_to_string(std::io::stdin())?,
                (false, Some(path)) => std::fs::read_to_string(path)?,
                _ => text.join(" "),
            };
            store
                .comment(&board, &task, &body, reply.as_deref())
                .await
                .map_err(anyhow::Error::new)?;
            println!("коммент записан: issue #{}", task.id);
            Ok(())
        }
        TaskSub::Thread { id } => {
            let task = store
                .resolve(&board, &id)
                .await
                .map_err(anyhow::Error::new)?;
            let entries = store.thread(&board, &task).await?;
            if entries.is_empty() {
                println!("комментов нет");
                return Ok(());
            }
            for e in &entries {
                match &e.reply_to {
                    Some(anchor) => println!("  ↳ [{}] {}: {}", anchor, e.author, e.text),
                    None => println!("[{}] {}: {}", e.id, e.author, e.text),
                }
            }
            Ok(())
        }
        TaskSub::Close { id, note } => {
            let task = store
                .resolve(&board, &id)
                .await
                .map_err(anyhow::Error::new)?;
            let closed = store.close(&board, &task, note.as_deref()).await?;
            println!("закрыта: issue #{} «{}»", closed.id, closed.title);
            Ok(())
        }
        TaskSub::Body {
            id,
            stdin,
            md,
            replace,
            base,
            force,
            attach,
            replace_section,
        } => {
            if let Some(path) = &md {
                path_flag("--md", path, "--stdin")?;
            }
            if let Some(base_path) = &base {
                path_flag("--base", base_path, "--force")?;
            }
            for path in &attach {
                path_flag("--attach", path.to_str().unwrap_or(""), "--stdin")?;
            }
            let task = store
                .resolve(&board, &id)
                .await
                .map_err(anyhow::Error::new)?;
            let new_body = match (stdin, &md) {
                (true, _) => Some(std::io::read_to_string(std::io::stdin())?),
                (false, Some(path)) => Some(std::fs::read_to_string(path)?),
                _ => None,
            };
            // вход — контент тела: обёртка task get и fm-карточка в тело не
            // вставляются (frontmatter ведёт store, шапка — печать get;
            // help-контракт «frontmatter-шапка — отказ») — отказ ДО мутации.
            // Append-ветка мягче: "---" первой строкой может быть легитимной
            // линейкой контента, отказ только на полный fm-блок.
            if let Some(md) = &new_body {
                // полный fm-блок: "---\n" первой строкой + закрывающая "---"
                // ниже (саму первую строку не считаем — "---\n<текст>" без
                // закрывающей это легитимная линейка контента)
                let full_fm_block = md.starts_with("---\n")
                    && md
                        .split('\n')
                        .skip(1)
                        .take(29)
                        .any(|l| l.trim_end() == "---");
                let wrap = md.starts_with("Задача:");
                if wrap || (replace && md.starts_with("---\n")) || (!replace && full_fm_block) {
                    bail!("вход task body — контент тела: frontmatter-шапка и обёртка task get в тело не вставляются")
                }
            }
            match (replace, new_body) {
                (true, Some(md)) => {
                    if !force && base.is_none() {
                        bail!("task body --replace требует --base (снапшот свежести) или --force");
                    }
                    // base — содержимое снапшота (файл): защита от
                    // параллельной правки = сверка base vs текущее тело.
                    // --force: сверка тривиально проходит (текущее тело
                    // подставляется как base) — свежесть не проверяется.
                    let base_md = if force {
                        store.body_get(&board, &task).await?
                    } else {
                        match &base {
                            Some(path) => std::fs::read_to_string(path)?,
                            None => String::new(),
                        }
                    };
                    if let Some(section) = &replace_section {
                        bail!(
                            "task body --replace-section «{section}»: на hub-бекенде секционная \
                             хирургия не поддерживается — правь body (--replace/--force) целиком"
                        );
                    }
                    store.body_replace(&board, &task, &md, &base_md).await?;
                    if !attach.is_empty() {
                        // ассеты hub: assets/ репо uwuwu-hub (ссылка raw-URL
                        // руками в теле) — загрузка через cli не портирована
                        println!(
                            "⚠ --attach на hub не портирован: файлы не загружены — \
                             положи в assets/ репо uwuwu-hub и вставь raw-ссылки"
                        );
                    }
                    println!("тело заменено: issue #{}", task.id);
                }
                (true, None) => bail!("task body --replace: вход пуст (stdin/--md)"),
                (false, Some(md)) => {
                    let current = store.body_get(&board, &task).await?;
                    let mut next = current.clone();
                    if !next.ends_with('\n') {
                        next.push('\n');
                    }
                    next.push_str(&md);
                    if !next.ends_with('\n') {
                        next.push('\n');
                    }
                    store.body_replace(&board, &task, &next, &current).await?;
                    println!("дописано в тело: issue #{}", task.id);
                }
                _ => {
                    let current = store.body_get(&board, &task).await?;
                    println!("{current}");
                }
            }
            Ok(())
        }
        other => {
            let _ = other;
            not_migrated("rm/clean/undo/cron-семья")
        }
    }
}

/// Шаблон тела задачи по типу (тело-шаблон create —
/// утилита домен-нейтральна, мигрировала с store-вырезкой).
pub fn create_body_md(title: &str, ty: &str, _ctx: &str) -> String {
    format!("# {title}\n\n\n<!-- task-type: {ty} -->\n")
}
