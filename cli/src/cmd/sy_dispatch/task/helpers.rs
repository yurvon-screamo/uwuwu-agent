//! Общие хелперы task-семьи: резолв задач (с --project и глобально),
//! формат шапки карточки, контракт ответов (read-after-write).

use crate::md_artifacts;
use crate::siyuan::SiYuan;
use crate::siyuan_task as syt;
use crate::siyuan_task::SyTask;
use anyhow::{anyhow, Result};

/// Резолв задачи для id-команд: с --project — доска проекта, без — глобально
/// по доскам всех проектов (уникальность обязательна, раскладка «доска →
/// совпадения» в ошибках).
pub(super) async fn resolve_task(
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

/// Шапка карточки задачи (контракт ответов и task get — один формат).
/// Синхронный форматтер: значения колонок берутся из перечитанной строки,
/// расписание/агент — из атрибутов тела (None — не читать).
pub(crate) fn format_task_header(
    board: &syt::SyBoard,
    task: &SyTask,
    schedule: Option<&str>,
    agent: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("Задача:   {}\n", task.title));
    out.push_str(&format!("Статус:   {}\n", task.status));
    if !task.task_type.is_empty() {
        out.push_str(&format!("Тип:      {}\n", task.task_type));
    }
    if !task.priority.is_empty() {
        out.push_str(&format!("Приоритет: {}\n", task.priority));
    }
    if task.autodone {
        out.push_str("Autodone: on (автомерж PR при входе в Review)\n");
    }
    out.push_str(&format!("Строка:   {}\n", task.item_id));
    // где приземлился резолв — особенно при глобальном (без --project)
    out.push_str(&format!("Доска:    {}\n", board.hpath));
    if !task.branch.is_empty() {
        out.push_str(&format!("Ветка:    {}\n", task.branch));
    }
    if !task.pr.is_empty() {
        out.push_str(&format!("PR:       {}\n", task.pr));
    }
    if !task.dev_session.is_empty() {
        out.push_str(&format!("Сессия dev: {}\n", task.dev_session));
    }
    if !task.planned.is_empty() {
        out.push_str(&format!("Запланировано: {}\n", task.planned));
    }
    if let Some(schedule) = schedule {
        out.push_str(&format!(
            "Расписание: {schedule} (крон: файр в статусе Вейт)\n"
        ));
    }
    if let Some(agent) = agent {
        out.push_str(&format!("{}\n", syt::agent_display_line(agent)));
    }
    out
}

/// Шапка с живыми атрибутами тела (расписание/агент): общий хвост task get
/// и мутаций. Резолв тела громкий (task_body_doc: привязка + fallback по
/// заголовку; отказ при телах-дублях — «вычисти сироты в UI»): get печатает
/// диагноз по `?`, мутации зовут в ADR-067-обёртке (Err → warning, rc 0).
/// Чтение attrs тихое (паритет get: колонки важнее расписания).
pub(super) async fn task_header_md(
    sy: &SiYuan,
    board: &syt::SyBoard,
    task: &SyTask,
) -> Result<String> {
    // один резолв на вызов: warning мёртвой привязки печатается ровно один раз
    let body_doc = syt::task_body_doc(sy, board, task).await?;
    task_header_md_with_doc(sy, board, task, body_doc.as_deref()).await
}

/// Формат шапки по уже резолвленному телу (Single-резолв контракт: get
/// резолвит один раз и передаёт док в шапку и экспорт — двойной warning
/// мёртвой привязки недопустим, ревью раунд 3).
pub(super) async fn task_header_md_with_doc(
    sy: &SiYuan,
    board: &syt::SyBoard,
    task: &SyTask,
    body_doc: Option<&str>,
) -> Result<String> {
    let mut schedule = None;
    let mut agent = None;
    if let Some(doc) = body_doc {
        if let Ok(attrs) = sy.get_block_attrs(doc).await {
            schedule = attrs.get("custom-schedule").cloned();
            agent = attrs.get("custom-agent").cloned();
        }
    }
    Ok(format_task_header(
        board,
        task,
        schedule.as_deref(),
        agent.as_deref(),
    ))
}

/// Перечитывание строки после мутации (read-after-write): render доски
/// заново, поиск по itemID. NotFound — отказ (вызывающий решает: ADR-067
/// мутация применена → warning, не падение).
pub(crate) async fn reload_task(
    sy: &SiYuan,
    board: &syt::SyBoard,
    item_id: &str,
) -> Result<SyTask> {
    let rows = syt::read_rows(sy, board).await?;
    rows.into_iter()
        .find(|r| r.item_id == item_id)
        .ok_or_else(|| anyhow!("строка {item_id} не видна в render доски «{}»", board.hpath))
}

/// Финальный ответ мутации: шапка перечитанной строки. Ветка ADR-067
/// («мутация применена — после неё не падаем»): перечитывание сорвалось
/// (транспорт/лаг render) → rc 0 + warning + рецепт контрольного get,
/// чтобы агент не ретраил уже применённую мутацию.
pub(super) async fn print_final_state(sy: &SiYuan, board: &syt::SyBoard, task: &SyTask) {
    match reload_task(sy, board, &task.item_id).await {
        Ok(fresh) => match task_header_md(sy, board, &fresh).await {
            Ok(header) => println!("{header}"),
            Err(err) => warn_unreadable_state(&task.item_id, &err),
        },
        Err(err) => warn_unreadable_state(&task.item_id, &err),
    }
}

pub(crate) fn warn_unreadable_state(item_id: &str, err: &anyhow::Error) {
    eprintln!(
        "⚠ мутация применена, но финальное состояние не прочитано ({err:#}) — \
         сверяй контрольным: task get {item_id}"
    );
}

/// Шапка из факта create (ADR-067-окно: render лагает/сорвался): item/title/
/// тип/начальный статус известны без перечитывания — stdout сохраняет
/// «Строка: <id>» для потребителей stdout (крон-ран-карточки), warning
/// требует контрольный get.
pub(crate) fn created_header(
    board: &syt::SyBoard,
    item: &str,
    title: &str,
    ty: &str,
    initial_status: &str,
) -> String {
    let draft = SyTask {
        item_id: item.to_string(),
        title: title.to_string(),
        status: initial_status.to_string(),
        task_type: ty.to_string(),
        priority: String::new(),
        autodone: false,
        branch: String::new(),
        pr: String::new(),
        dev_session: String::new(),
        planned: String::new(),
        planned_ms: 0,
        schedule_cell: String::new(),
        body_doc: None,
    };
    format_task_header(board, &draft, None, None)
}

/// Финальный экспорт тела после мутации (контракт «фулл-тело»): маркер-блок
/// и artifact-report. Create/body печатают канон-маркер «----- тело -----»
/// (ровно как get: write-гард task_get_wrapper_signature и срезка --base
/// покрывают рециркуляцию ответа тем же матчем); goal/person — «(итог)»-
/// маркеры (расширение гарда). Экспорт сорвался → маркер без тела + warning
/// (rc 0, ADR-067: запись уже свершилась).
pub(crate) async fn print_final_body(sy: &SiYuan, doc_id: &str, label: &str) {
    match sy.export_md_by_id(doc_id).await {
        Ok(md) => {
            for warning in md_artifacts::artifact_report(&md) {
                eprintln!("⚠ round-trip-артефакт: {warning}");
            }
            println!("\n----- {label} -----\n{md}");
        }
        Err(err) => {
            eprintln!(
                "⚠ запись применена, но финальный экспорт не получен ({err:#}) — \
                 сверяй контрольным чтением дока {doc_id}"
            );
        }
    }
}
