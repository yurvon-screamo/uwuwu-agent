//! Персистентный undo-журнал чистки (#1158): JSON-файл (volume, рядом со
//! state.json), атомарная запись. Реализация порта CleanJournalStore.
//! Журналы переживают рестарт — restore возможен и после деплоя (ADR-025:
//! «журналы переживают revert кода»). Кап: старейшие журналы срезаются —
//! undo живёт в разумном окне, файл не растёт вечно.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use uwuwu_usecase::{CleanJournal, CleanJournalStore, UsecaseError, UsecaseResult};

/// Хранитель: cap по числу журналов (глобально, свежие первыми).
const JOURNAL_CAP: usize = 100;

#[derive(Debug, Default, Serialize, Deserialize)]
struct JournalFile {
    /// id журнала → журнал (BTreeMap — детерминированный порядок в файле).
    #[serde(default)]
    journals: BTreeMap<String, CleanJournal>,
}

pub struct JsonCleanJournals {
    path: PathBuf,
    file: std::sync::Mutex<JournalFile>,
}

impl JsonCleanJournals {
    pub fn load(path: PathBuf) -> Self {
        let file: JournalFile = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        Self {
            path,
            file: std::sync::Mutex::new(file),
        }
    }

    /// Атомарная запись (tmp + rename — единый паттерн state.json).
    fn save_now(&self, file: &JournalFile) -> UsecaseResult<()> {
        let raw = serde_json::to_vec_pretty(file)
            .map_err(|e| UsecaseError::State(format!("сериализация журнала: {e}")))?;
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

impl CleanJournalStore for JsonCleanJournals {
    fn put(&self, journal: &CleanJournal) -> UsecaseResult<()> {
        let mut guard = self.file.lock().expect("journal lock");
        guard.journals.insert(journal.id.clone(), journal.clone());
        // кап: старейшие по created_ms срезаются (undo живёт в окне,
        // файл не растёт вечно)
        if guard.journals.len() > JOURNAL_CAP {
            let mut by_age: Vec<(u64, String)> = guard
                .journals
                .values()
                .map(|j| (j.created_ms, j.id.clone()))
                .collect();
            by_age.sort_unstable();
            let cut = guard.journals.len() - JOURNAL_CAP;
            for (_, id) in by_age.into_iter().take(cut) {
                guard.journals.remove(&id);
            }
        }
        self.save_now(&guard)
    }

    fn get(&self, project: &str, id: Option<&str>) -> UsecaseResult<Option<CleanJournal>> {
        let guard = self.file.lock().expect("journal lock");
        match id {
            Some(id) => Ok(guard
                .journals
                .get(id)
                .filter(|j| j.project == project)
                .cloned()),
            None => Ok(guard
                .journals
                .values()
                .filter(|j| j.project == project)
                .next_back()
                .cloned()),
        }
    }

    fn ids(&self, project: &str) -> UsecaseResult<Vec<String>> {
        let guard = self.file.lock().expect("journal lock");
        let mut ids: Vec<String> = guard
            .journals
            .values()
            .filter(|j| j.project == project)
            .map(|j| j.id.clone())
            .collect();
        ids.reverse();
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uwuwu_usecase::CleanRow;

    fn journal(id: &str, created_ms: u64) -> CleanJournal {
        CleanJournal {
            id: id.into(),
            project: "uwuwu".into(),
            board: "Задачи uwuwu".into(),
            created_ms,
            rows: vec![CleanRow {
                number: 7,
                title: "t".into(),
                status_raw: Some("Draft".into()),
                item_id: 42,
            }],
        }
    }

    fn store(path: PathBuf) -> JsonCleanJournals {
        JsonCleanJournals::load(path)
    }

    #[test]
    fn round_trip_through_file_and_latest() {
        let dir = std::env::temp_dir().join(format!("uwuwu-undo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("undo.json");
        {
            let s = store(path.clone());
            CleanJournalStore::put(&s, &journal("old-1", 100)).unwrap();
            CleanJournalStore::put(&s, &journal("fresh-2", 200)).unwrap();
        }
        let s = store(path.clone());
        // свежий (id None) и поимённый доступ
        let latest = CleanJournalStore::get(&s, "uwuwu", None).unwrap().unwrap();
        assert_eq!(latest.id, "fresh-2");
        assert_eq!(
            CleanJournalStore::get(&s, "uwuwu", Some("old-1"))
                .unwrap()
                .unwrap()
                .id,
            "old-1"
        );
        assert_eq!(CleanJournalStore::ids(&s, "uwuwu").unwrap(), vec!["fresh-2", "old-1"]);
        // чужой проект не отдаётся
        assert!(CleanJournalStore::get(&s, "другой", None).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_fresh_store() {
        let s = store(PathBuf::from("/nonexistent/uwuwu/undo.json"));
        assert!(CleanJournalStore::get(&s, "uwuwu", None).unwrap().is_none());
        assert!(CleanJournalStore::ids(&s, "uwuwu").unwrap().is_empty());
    }

    #[test]
    fn cap_cuts_oldest_journals() {
        let dir = std::env::temp_dir().join(format!("uwuwu-undo-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("undo.json");
        let s = store(path.clone());
        for n in 0..(JOURNAL_CAP + 10) {
            CleanJournalStore::put(&s, &journal(&format!("j{n}"), 1_000 + n as u64)).unwrap();
        }
        let ids = CleanJournalStore::ids(&s, "uwuwu").unwrap();
        assert_eq!(ids.len(), JOURNAL_CAP, "кап держит размер");
        assert!(
            !ids.contains(&"j0".to_string()) && !ids.contains(&"j9".to_string()),
            "старейшие срезаны"
        );
        assert!(ids.contains(&format!("j{}", JOURNAL_CAP + 9)), "свежий жив");
        std::fs::remove_dir_all(&dir).ok();
    }
}
