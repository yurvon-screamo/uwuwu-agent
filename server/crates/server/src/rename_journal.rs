//! Write-ahead pre-image журнал rename статуса (#1447): JSON-файл рядом
//! со state.json (volume), атомарная запись — паттерн clean_journal.
//! Хранит ПОСЛЕДНИЙ pre-image на доску (перезапись: restore-ценность у
//! свежего снимка, история не копится). Переживает деплой/реверт —
//! материал ручного отката (путь восстановления #1441).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use uwuwu_usecase::{StatusPreImage, StatusPreImageStore, UsecaseError, UsecaseResult};

#[derive(Debug, Default, Serialize, Deserialize)]
struct PreImageFile {
    /// доска → последний pre-image (BTreeMap — детерминированный порядок).
    #[serde(default)]
    by_board: BTreeMap<String, StatusPreImage>,
}

pub struct JsonStatusPreImages {
    path: PathBuf,
    file: std::sync::Mutex<PreImageFile>,
}

impl JsonStatusPreImages {
    pub fn load(path: PathBuf) -> Self {
        // повреждённый файл НЕ глотается молча (ревью #1158 Low-2):
        // pre-image — safety-net rename, тихая потеря снимка недопустима;
        // стартуем пустым с громкой строкой в логе
        let file = match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<PreImageFile>(&raw) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(
                        event = "rename_preimage.corrupt",
                        path = %path.display(),
                        error = %e,
                        "rename-pre-image.json не парсится — pre-image снимки \
                         потеряны (откат возможен только по истории носителя), \
                         стартуем пустым"
                    );
                    PreImageFile::default()
                }
            },
            // нет файла — норма (первый запуск / ещё ни одного rename)
            Err(_) => PreImageFile::default(),
        };
        Self {
            path,
            file: std::sync::Mutex::new(file),
        }
    }

    /// Атомарная запись (tmp + rename — единый паттерн state.json).
    fn save_now(&self, file: &PreImageFile) -> UsecaseResult<()> {
        let raw = serde_json::to_vec_pretty(file)
            .map_err(|e| UsecaseError::State(format!("сериализация pre-image: {e}")))?;
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

impl StatusPreImageStore for JsonStatusPreImages {
    fn put(&self, image: &StatusPreImage) -> UsecaseResult<()> {
        let mut guard = self.file.lock().expect("pre-image lock");
        guard.by_board.insert(image.board.clone(), image.clone());
        self.save_now(&guard)
    }

    fn path_hint(&self) -> String {
        self.path.display().to_string()
    }
}
