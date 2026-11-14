//! Персистентное состояние тика: JSON-файл (volume), атомарная запись.
//! Реализация порта TickStateStore.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use uwuwu_usecase::error::{UsecaseError, UsecaseResult};
use uwuwu_usecase::TickStateStore;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RowState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_comment_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StateFile {
    /// «board:number» → состояние строки.
    #[serde(default)]
    rows: BTreeMap<String, RowState>,
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
}

impl TickStateStore for JsonStateStore {
    fn last_comment_id(&self, board: &str, number: u64) -> Option<String> {
        let guard = self.state.lock().expect("state lock");
        guard
            .rows
            .get(&Self::key(board, number))
            .and_then(|r| r.last_comment_id.clone())
    }

    fn set_last_comment_id(&self, board: &str, number: u64, id: &str) {
        let mut guard = self.state.lock().expect("state lock");
        guard
            .rows
            .entry(Self::key(board, number))
            .or_default()
            .last_comment_id = Some(id.to_string());
    }

    fn save(&self) -> UsecaseResult<()> {
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
            s.save().unwrap();
        }
        let s = JsonStateStore::load(path.clone());
        assert_eq!(s.last_comment_id("Задачи uwuwu", 7).as_deref(), Some("c1"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_fresh_state() {
        let s = JsonStateStore::load(PathBuf::from("/nonexistent/uwuwu/x.json"));
        assert!(s.last_comment_id("b", 1).is_none());
    }
}
