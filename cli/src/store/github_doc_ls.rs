//! docs ls: перечень детей папки клона + отказ unknown-пути с раскладкой
//! доступных (help-контракт `DocsSub::Ls`; задача 1094).

use super::doc_store::DocEntry;
use super::github_doc_store::GitHubDocStore;
use anyhow::anyhow;
use std::io::ErrorKind;
use std::path::Path;

/// Дети папки клона: имя (+ «/» для папок), «.git» скипается, сортировка
/// по имени — единый формат перечня ls (хеппи-путь и раскладка отказа).
pub(crate) fn children_sorted(dir: &Path) -> std::io::Result<Vec<DocEntry>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == ".git" {
            continue;
        }
        let is_folder = entry.file_type()?.is_dir();
        out.push(DocEntry {
            name: match is_folder {
                true => format!("{name}/"),
                false => name,
            },
            is_folder,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// true для io-кодов «путь не существует» и «путь — файл»: ls обрабатывает
/// их отказом с раскладкой (`missing_hint`); остальные io-коды наружу как есть.
pub(crate) fn is_ls_missing_kind(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
}

/// Небезопасный путь (..-ходы) — громкий отказ без резолва.
pub(crate) fn unsafe_path_error(path: &str) -> anyhow::Error {
    anyhow!("путь «{path}» небезопасен")
}

impl GitHubDocStore {
    /// Отказ unknown-пути: ближайший существующий предок пути внутри docs/ —
    /// папка даёт раскладку детей («доступно в …»), файл — read-хинт со слагом
    /// (путь в docs/ без «.md»); docs/ отсутствует — честный отказ о клоне,
    /// не маскировка под «не найдено».
    pub(crate) fn missing_hint(&self, clean: &str, path: &str) -> anyhow::Error {
        let docs = self.clone_root.join("docs");
        if !docs.is_dir() {
            return anyhow!(
                "git-клон {} без docs/ — клон пуст или сломан, разбери вручную",
                self.clone_root.display()
            );
        }
        let segs: Vec<&str> = clean.split('/').filter(|s| !s.is_empty()).collect();
        for k in (0..=segs.len()).rev() {
            let rel = segs[..k].join("/");
            let cand = docs.join(&rel);
            if !cand.exists() {
                continue;
            }
            if cand.is_file() {
                let slug = rel.strip_suffix(".md").unwrap_or(&rel);
                return anyhow!(
                    "путь «{path}» упирается в файл «{rel}» — ls перечисляет папку; \
                     читай: uwuwu-cli read \"{slug}\""
                );
            }
            let names: Vec<String> = match children_sorted(&cand) {
                Ok(entries) => entries.into_iter().map(|e| e.name).collect(),
                Err(e) => return anyhow::Error::new(e),
            };
            let scope = if rel.is_empty() { "docs" } else { rel.as_str() };
            if names.is_empty() {
                return anyhow!("путь «{path}» не найден в docs/; «{scope}» пуст");
            }
            return anyhow!(
                "путь «{path}» не найден в docs/; доступно в «{scope}»: {}",
                names.join(", ")
            );
        }
        anyhow!("путь «{path}» не найден в docs/")
    }
}
