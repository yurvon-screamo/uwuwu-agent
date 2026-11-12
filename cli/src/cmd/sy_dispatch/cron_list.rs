//! cron list: строки инвентаря мастеров крон-задач доски проекта.

use crate::cmd::sy_dispatch::cron::{format_local, last_run_from_board, read_cron_state};
use crate::cmd::sy_dispatch::truncate;
use crate::siyuan::SiYuan;
use crate::siyuan_task as syt;
use anyhow::Result;

/// Строка инвентаря мастеров (поля таблицы cron list).
struct CronMasterRow {
    status: String,
    row: String,
    title: String,
    schedule: String,
    agent: String,
    planned: String,
    last_run: String,
}

/// Инвентарь мастеров крон-задач доски проекта: строки с custom-schedule
/// в доке-теле. Скедул — атрибут; last run — доска (факт файра в клетках
/// ран-карточек мастера), state демона — фоллэк; «Запланировано» — клетка
/// мастера (next-fire). Возвращает готовые строки таблицы — pub(crate)
/// для тестов (содержимое таблицы проверяется, не только отсутствие отказа).
pub(crate) async fn cron_list(sy: &SiYuan, project: &str) -> Result<Vec<String>> {
    let board = syt::find_board(sy, project).await?;
    let rows = syt::read_rows(sy, &board).await?;
    let state = read_cron_state();
    let mut masters: Vec<CronMasterRow> = Vec::new();
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
        let last_run = match last_run_from_board(&r.title, &rows) {
            Some(ms) => format_local(ms as u64),
            None => state
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
                .unwrap_or_default(),
        };
        masters.push(CronMasterRow {
            status: r.status.clone(),
            row: r.item_id.chars().take(12).collect(),
            title: truncate(&r.title, 26),
            schedule: truncate(schedule, 13),
            agent: truncate(&agent, 12),
            planned: r.planned.clone(),
            last_run,
        });
    }
    if masters.is_empty() {
        return Ok(vec![
            "Крон-задач нет (расписание — task create/edit --schedule, статус Вейт).".to_string(),
        ]);
    }
    // ширина колонки по факту: клетки в formattedContent ядра бывают шире
    // заголовка (UI-локаль «11 ноября 2026 г. 01:30») — рвать таблицу на
    // фиксированных {:<17} не надо
    let planned_w = masters
        .iter()
        .map(|m| m.planned.chars().count())
        .chain(["ЗАПЛАНИРОВАНО".chars().count()])
        .max()
        .unwrap_or_default()
        + 1;
    let mut out = vec![format!(
        "{:<10} {:<12} {:<28} {:<14} {:<13} {:<plw$} LAST RUN",
        "СТАТУС",
        "СТРОКА",
        "ЗАГОЛОВОК",
        "SCHEDULE",
        "AGENT",
        "ЗАПЛАНИРОВАНО",
        plw = planned_w,
    )];
    for m in masters {
        out.push(format!(
            "{:<10} {:<12} {:<28} {:<14} {:<13} {:<plw$} {}",
            m.status,
            m.row,
            m.title,
            m.schedule,
            m.agent,
            m.planned,
            m.last_run,
            plw = planned_w,
        ));
    }
    Ok(out)
}
