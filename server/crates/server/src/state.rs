//! Персистентное состояние тика: JSON-файл (volume), атомарная запись.
//! Реализация порта TickStateStore.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use uwuwu_domain::{Slot, Status};
use uwuwu_usecase as naming;
use uwuwu_usecase::BotStateStore;
use uwuwu_usecase::TickStateStore;
use uwuwu_usecase::{UsecaseError, UsecaseResult};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RowState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_comment_id: Option<String>,
    /// Последний статус, виденный тиком (детект переходов стадии —
    /// канон сессий: ревью/тест-раунды всегда со свежей сессией).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_status: Option<String>,
    /// Сталл-детект: (статус раунда, ms пинга, счётчик) — смена статуса
    /// обнуляется записью нового статуса (см. mark_stall_ping).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stall: Option<(String, u64, u32)>,
}

/// Состояние бота (секция `bots`): вечная сессия + offset апдейтов ТГ.
/// Сессия переживает stop/re-arm; пересоздание — clear session, offset
/// остаётся (поллинг продолжается с непрочитанного).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct BotRow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    offset: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    manual_hash: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StateFile {
    /// «board:number» → состояние строки.
    #[serde(default)]
    rows: BTreeMap<String, RowState>,
    /// Сессии, отработанные дримером.
    #[serde(default)]
    dreamer_done: Vec<String>,
    /// «board:number:slot» → ms последнего спавна (анти-шторм).
    #[serde(default)]
    spawns: BTreeMap<String, u64>,
    /// «board:number:slot» → (статус на прошлом спавне, повторов) —
    /// attention-механика.
    #[serde(default)]
    spawn_loops: BTreeMap<String, (String, u32)>,
    /// «board:number» → (статус цикла, кругов) — цикл-гвард аналитики (#1135).
    #[serde(default)]
    analys_loops: BTreeMap<String, (String, u32)>,
    /// Дневной счётчик дримера: (дата JST, спавнов).
    #[serde(default)]
    dreamer_day: Option<(String, u32)>,
    /// «slug бота» → сессия + offset (раннер ботов, #1150).
    #[serde(default)]
    bots: BTreeMap<String, BotRow>,
}

pub struct JsonStateStore {
    path: PathBuf,
    state: std::sync::Mutex<StateFile>,
}

impl JsonStateStore {
    pub fn load(path: PathBuf) -> Self {
        let state = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        Self {
            path,
            state: std::sync::Mutex::new(state),
        }
    }

    fn key(board: &str, number: u64) -> String {
        format!("{board}:{number}")
    }

    /// Атомарная запись файла (единая точка: оба трейта — делегаты).
    fn save_now(&self) -> UsecaseResult<()> {
        // гигиена роста: dreamer_done — последние 500
        {
            let mut guard = self.state.lock().expect("state lock");
            if guard.dreamer_done.len() > 500 {
                let cut = guard.dreamer_done.len() - 500;
                guard.dreamer_done.drain(0..cut);
            }
            if guard.spawns.len() > 5000 {
                let keys: Vec<String> = guard.spawns.keys().cloned().collect();
                let cut = guard.spawns.len() - 5000;
                for k in keys.into_iter().take(cut) {
                    guard.spawns.remove(&k);
                }
            }
            if guard.spawn_loops.len() > 5000 {
                let keys: Vec<String> = guard.spawn_loops.keys().cloned().collect();
                let cut = guard.spawn_loops.len() - 5000;
                for k in keys.into_iter().take(cut) {
                    guard.spawn_loops.remove(&k);
                }
            }
        }
        let raw = {
            let guard = self.state.lock().expect("state lock");
            serde_json::to_vec_pretty(&*guard)
        };
        let raw = raw.map_err(|e| UsecaseError::State(format!("сериализация: {e}")))?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| UsecaseError::State(format!("mkdir {}: {e}", dir.display())))?;
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, &raw)
            .map_err(|e| UsecaseError::State(format!("write {}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| UsecaseError::State(format!("rename {}: {e}", self.path.display())))?;
        Ok(())
    }
}

impl TickStateStore for JsonStateStore {
    fn last_comment_id(&self, board: &str, number: u64) -> Option<String> {
        let guard = self.state.lock().expect("state lock");
        guard
            .rows
            .get(&Self::key(board, number))
            .and_then(|r| r.last_comment_id.clone())
    }

    fn last_seen_status(&self, board: &str, number: u64) -> Option<Status> {
        let guard = self.state.lock().expect("state lock");
        guard
            .rows
            .get(&Self::key(board, number))
            .and_then(|r| r.last_status.as_deref())
            .map(naming::parse_status)
    }

    fn set_last_seen_status(&self, board: &str, number: u64, status: Status) {
        let mut guard = self.state.lock().expect("state lock");
        let name = naming::status_name(status).to_string();
        guard
            .rows
            .entry(Self::key(board, number))
            .or_default()
            .last_status = Some(name);
    }

    fn stall_ping(&self, board: &str, number: u64) -> Option<(uwuwu_domain::Status, u64, u32)> {
        let guard = self.state.lock().expect("state lock");
        guard
            .rows
            .get(&Self::key(board, number))
            .and_then(|r| r.stall.as_ref())
            .map(|(st, ms, c)| (naming::parse_status(st), *ms, *c))
    }

    fn mark_stall_ping(&self, board: &str, number: u64, status: uwuwu_domain::Status, at_ms: u64) {
        let mut guard = self.state.lock().expect("state lock");
        let row = guard.rows.entry(Self::key(board, number)).or_default();
        // счётчик растёт внутри раунда (одинаковый статус); смена статуса = 1
        let count = match &row.stall {
            Some((st, _, c)) if naming::parse_status(st) == status => *c + 1,
            _ => 1,
        };
        row.stall = Some((naming::status_name(status).to_string(), at_ms, count));
    }

    fn set_last_comment_id(&self, board: &str, number: u64, id: &str) {
        let mut guard = self.state.lock().expect("state lock");
        guard
            .rows
            .entry(Self::key(board, number))
            .or_default()
            .last_comment_id = Some(id.to_string());
    }

    fn dreamer_done(&self, session: &str) -> bool {
        self.state
            .lock()
            .expect("state lock")
            .dreamer_done
            .iter()
            .any(|s| s == session)
    }

    fn mark_dreamer_done(&self, session: &str) {
        self.state
            .lock()
            .expect("state lock")
            .dreamer_done
            .push(session.to_string());
    }

    fn dreamer_day_spawns(&self, day_key: &str) -> u32 {
        self.state
            .lock()
            .expect("state lock")
            .dreamer_day
            .as_ref()
            .filter(|(d, _)| d == day_key)
            .map(|(_, n)| *n)
            .unwrap_or(0)
    }

    fn mark_dreamer_spawn(&self, day_key: &str) {
        let mut guard = self.state.lock().expect("state lock");
        let n = guard
            .dreamer_day
            .as_ref()
            .filter(|(d, _)| d == day_key)
            .map(|(_, n)| *n)
            .unwrap_or(0);
        guard.dreamer_day = Some((day_key.to_string(), n + 1));
    }

    fn last_spawn_ms(&self, board: &str, number: u64, slot: Slot) -> Option<u64> {
        let key = format!("{board}:{number}:{}", naming::slot_key(slot));
        self.state
            .lock()
            .expect("state lock")
            .spawns
            .get(&key)
            .copied()
    }

    fn clear_spawn_ms(&self, board: &str, number: u64, slot: Slot) {
        self.state
            .lock()
            .expect("state lock")
            .spawns
            .remove(&format!("{board}:{number}:{}", naming::slot_key(slot)));
    }

    fn mark_spawn(&self, board: &str, number: u64, slot: Slot, at_ms: u64) {
        self.state.lock().expect("state lock").spawns.insert(
            format!("{board}:{number}:{}", naming::slot_key(slot)),
            at_ms,
        );
    }

    fn last_analys_ms(&self, board: &str, number: u64) -> Option<u64> {
        self.state
            .lock()
            .expect("state lock")
            .spawns
            .get(&format!("{board}:{number}:analys"))
            .copied()
    }

    fn mark_analys_spawn(&self, board: &str, number: u64, at_ms: u64) {
        self.state
            .lock()
            .expect("state lock")
            .spawns
            .insert(format!("{board}:{number}:analys"), at_ms);
    }

    fn last_analys_review_ms(&self, board: &str, number: u64) -> Option<u64> {
        self.state
            .lock()
            .expect("state lock")
            .spawns
            .get(&format!("{board}:{number}:analys-review"))
            .copied()
    }

    fn mark_analys_review_spawn(&self, board: &str, number: u64, at_ms: u64) {
        self.state
            .lock()
            .expect("state lock")
            .spawns
            .insert(format!("{board}:{number}:analys-review"), at_ms);
    }

    fn analys_loop(&self, board: &str, number: u64) -> Option<(Status, u32)> {
        self.state
            .lock()
            .expect("state lock")
            .analys_loops
            .get(&Self::key(board, number))
            .map(|(s, r)| (naming::parse_status(s), *r))
    }

    fn set_analys_loop(&self, board: &str, number: u64, status: Status, rounds: u32) {
        self.state.lock().expect("state lock").analys_loops.insert(
            Self::key(board, number),
            (naming::status_name(status).to_string(), rounds),
        );
    }

    fn clear_analys_loop(&self, board: &str, number: u64) {
        self.state
            .lock()
            .expect("state lock")
            .analys_loops
            .remove(&Self::key(board, number));
    }
    fn spawn_loop(&self, board: &str, number: u64, slot: Slot) -> Option<(Status, u32)> {
        self.state
            .lock()
            .expect("state lock")
            .spawn_loops
            .get(&format!("{board}:{number}:{}", naming::slot_key(slot)))
            .map(|(s, r)| (naming::parse_status(s), *r))
    }

    fn set_spawn_loop(&self, board: &str, number: u64, slot: Slot, status: Status, repeats: u32) {
        self.state.lock().expect("state lock").spawn_loops.insert(
            format!("{board}:{number}:{}", naming::slot_key(slot)),
            (naming::status_name(status).to_string(), repeats),
        );
    }

    fn reset_stale_spawn_loops(&self, board: &str, number: u64, current: Status) {
        let prefix = format!("{board}:{number}:");
        let current_name = naming::status_name(current);
        let mut guard = self.state.lock().expect("state lock");
        let stale: Vec<String> = guard
            .spawn_loops
            .iter()
            .filter(|(k, (status, _))| k.starts_with(&prefix) && status != current_name)
            .map(|(k, _)| k.clone())
            .collect();
        for key in stale {
            guard.spawn_loops.remove(&key);
        }
    }

    fn save(&self) -> UsecaseResult<()> {
        self.save_now()
    }
}

impl BotStateStore for JsonStateStore {
    fn bot_session(&self, slug: &str) -> Option<String> {
        self.state
            .lock()
            .expect("state lock")
            .bots
            .get(slug)
            .and_then(|b| b.session.clone())
    }

    fn set_bot_session(&self, slug: &str, session: Option<&str>) {
        let mut guard = self.state.lock().expect("state lock");
        match session {
            // старая сессия в opencode.db живёт — забывается только ссылка
            Some(s) => guard.bots.entry(slug.to_string()).or_default().session = Some(s.into()),
            None => {
                if let Some(row) = guard.bots.get_mut(slug) {
                    row.session = None;
                }
            }
        }
    }

    fn bot_offset(&self, slug: &str) -> Option<u64> {
        self.state
            .lock()
            .expect("state lock")
            .bots
            .get(slug)
            .and_then(|b| b.offset)
    }

    fn bot_manual_hash(&self, slug: &str) -> Option<u64> {
        self.state
            .lock()
            .expect("state lock")
            .bots
            .get(slug)
            .and_then(|b| b.manual_hash)
    }

    fn set_bot_manual_hash(&self, slug: &str, hash: u64) {
        self.state
            .lock()
            .expect("state lock")
            .bots
            .entry(slug.to_string())
            .or_default()
            .manual_hash = Some(hash);
    }

    fn set_bot_offset(&self, slug: &str, offset: u64) {
        self.state
            .lock()
            .expect("state lock")
            .bots
            .entry(slug.to_string())
            .or_default()
            .offset = Some(offset);
    }

    fn save(&self) -> UsecaseResult<()> {
        self.save_now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_through_file() {
        let dir = std::env::temp_dir().join(format!("uwuwu-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        {
            let s = JsonStateStore::load(path.clone());
            s.set_last_comment_id("Задачи uwuwu", 7, "c1");
            // бот: сессия + offset переживают рестарт (секция bots)
            s.set_bot_session("Projects/uwuwu/Боты/x", Some("ses_abc"));
            s.set_bot_offset("Projects/uwuwu/Боты/x", 42);
            TickStateStore::save(&s).unwrap();
        }
        let s = JsonStateStore::load(path.clone());
        assert_eq!(s.last_comment_id("Задачи uwuwu", 7).as_deref(), Some("c1"));
        assert_eq!(
            s.bot_session("Projects/uwuwu/Боты/x").as_deref(),
            Some("ses_abc"),
            "рестарт восстанавливает сессию бота"
        );
        assert_eq!(s.bot_offset("Projects/uwuwu/Боты/x"), Some(42));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bot_session_clear_keeps_offset_row() {
        let s = JsonStateStore::load(PathBuf::from("/nonexistent/uwuwu/x.json"));
        s.set_bot_session("slug", Some("ses_1"));
        s.set_bot_offset("slug", 9);
        s.set_bot_session("slug", None);
        assert_eq!(s.bot_session("slug"), None, "сессия забыта");
        assert_eq!(
            s.bot_offset("slug"),
            Some(9),
            "offset переживает пересоздание"
        );
    }

    #[test]
    fn missing_file_is_fresh_state() {
        let s = JsonStateStore::load(PathBuf::from("/nonexistent/uwuwu/x.json"));
        assert!(s.last_comment_id("b", 1).is_none());
        assert!(s.bot_session("slug").is_none());
        assert!(s.bot_offset("slug").is_none());
    }
}
