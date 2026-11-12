//! cron: инвентарь крон-задач — мастеры с расписанием на доске задач,
//! state-file демона и форматирование времён.

use crate::cmd::sy_dispatch::cron_list::cron_list;
use crate::cmd::cli::CronSub;
use crate::siyuan::SiYuan;
use crate::siyuan_task::SyTask;
use anyhow::Result;

pub(crate) async fn cron_run(command: CronSub) -> Result<()> {
    let sy = SiYuan::from_env()?;
    let CronSub::List { project } = command;
    for line in cron_list(&sy, project.as_deref().unwrap_or("uwuwu")).await? {
        println!("{line}");
    }
    Ok(())
}

/// State демона uwuwu-cron: пер-машинский файл (~/.local/state/uwuwu-cron,
/// песочница двигает UWUWU_CRON_STATE_DIR). Отсутствует/бит — пусто: инвентарь
/// живёт на доске, state — только фоллэк last run (ранов на доске нет).
#[derive(serde::Deserialize, Default)]
pub(crate) struct CronStateFile {
    #[serde(default)]
    pub(crate) crons: std::collections::HashMap<String, TrackedCronEntry>,
}

/// Запись трекинга демона (формат cron.ts, camelCase: sessionID/lastFiredAt).
/// rename обязателен: дефолтный snake_case молча давал нули — LAST RUN
/// пуст при живых файрах (баг 20261110044120).
#[derive(serde::Deserialize, Default)]
pub(crate) struct TrackedCronEntry {
    #[serde(default, rename = "sessionID")]
    pub(crate) session_id: String,
    #[serde(default, rename = "lastFiredAt")]
    pub(crate) last_fired_at: u64,
}

/// Разбор state.json; битый/чужой JSON — пусто (read-only инвентарь не должен
/// падать на state демона). pub(crate) для тестов cron list.
pub(crate) fn parse_state(raw: &str) -> CronStateFile {
    serde_json::from_str(raw).unwrap_or_default()
}

pub(super) fn read_cron_state() -> CronStateFile {
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
    Ok(parse_state(&std::fs::read_to_string(path)?))
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

/// Ран-карточка мастера: тайтл «⏰ <мастер> · <дата>» (формат runCardTitle
/// демона) → Some(мастер) — структурный матч для сопоставления рана с
/// мастером. Хвост-дата не валидируется здесь: время файра берётся из
/// planned-клетки рана (свойство строки — канон 20261108020718).
pub(crate) fn run_card_owner(title: &str) -> Option<&str> {
    let rest = title.strip_prefix("⏰ ")?;
    let (owner, _tail) = rest.rsplit_once(" · ")?;
    Some(owner)
}

/// Фактический last run мастера с доски: максимум «Запланировано» (planned_ms
/// — date.content клетки, epoch ms) по его ран-карточкам — ран несёт
/// фактический файр (createRunCard). Строки без клетки (planned_ms = 0)
/// скипаются. None — ранов мастера на доске нет (фоллэк — state).
pub(crate) fn last_run_from_board(master_title: &str, rows: &[SyTask]) -> Option<i64> {
    rows.iter()
        .filter(|r| {
            run_card_owner(&r.title).is_some_and(|owner| owner.trim() == master_title.trim())
        })
        .map(|r| r.planned_ms)
        .filter(|ms| *ms > 0)
        .max()
}
