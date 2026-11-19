//! Реализации `BotAssetStore` (#1171): прод — коммит файла в приватный
//! hub-репо рядом с доком бота (`docs/bot/<бот>/…`, ff-only через
//! GitWikiWriter; раскладка /bot — #1214), e2e/memory — в-памяти
//! приёмник стенда. Нормализация имени — контракт usecase
//! (`normalize_asset_name`), здесь только путь и ссылка.

use std::sync::Mutex;
use uwuwu_usecase::bots::{normalize_asset_name, BotAssetStore, MediaKind};
use uwuwu_usecase::UsecaseResult;

use crate::wiki::{encode_path, GitWikiWriter};

/// Ссылка ассета: raw-URL приватного репо — владельцу открывается
/// браузерной сессией, агентам — gh. Инлайн-рендер в issue GitHub camo
/// не даёт (нет сессии) — компромисс плана #1171.
const RAW_BASE: &str = "https://raw.githubusercontent.com";

pub struct GitAssetStore {
    writer: std::sync::Arc<GitWikiWriter>,
    repo: String,
    branch: String,
}

impl GitAssetStore {
    pub fn new(writer: std::sync::Arc<GitWikiWriter>, repo: String, branch: String) -> Self {
        Self {
            writer,
            repo,
            branch,
        }
    }
}

/// Метка времени имени: unix-мс — уникальность внутри последовательной
/// обработки одного бота (HTTP-круг между put'ами ≫ 1 мс).
fn stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    now.to_string()
}

fn raw_url(repo: &str, branch: &str, path: &str) -> String {
    format!("{RAW_BASE}/{repo}/{branch}/{}", encode_path(path))
}

#[async_trait::async_trait]
impl BotAssetStore for GitAssetStore {
    async fn put(
        &self,
        bot: &str,
        kind: MediaKind,
        filename: Option<&str>,
        bytes: &[u8],
    ) -> UsecaseResult<String> {
        let bot_name = bot.rsplit('/').next().unwrap_or(bot);
        let st = stamp();
        let name = normalize_asset_name(filename, kind, &st);
        let path = format!("docs/bot/{bot_name}/{st}-{name}");
        self.writer
            .commit_bytes(&path, bytes, "bot asset (uwuwu #1171)")
            .await?;
        Ok(raw_url(&self.repo, &self.branch, &path))
    }
}

/// Стенд/память (e2e bots, memory-hub): фиксирует url↔байты, промпт
/// получает настоящую ссылку — полный media-флоу без GitHub.
/// Проверка стенда — по ТГ-отправкам и логам сервера, snapshot не нужен.
#[derive(Default)]
pub struct MemoryAssetStore {
    files: Mutex<Vec<(String, Vec<u8>)>>,
}

#[async_trait::async_trait]
impl BotAssetStore for MemoryAssetStore {
    async fn put(
        &self,
        bot: &str,
        kind: MediaKind,
        filename: Option<&str>,
        bytes: &[u8],
    ) -> UsecaseResult<String> {
        let bot_name = bot.rsplit('/').next().unwrap_or(bot);
        let st = stamp();
        let name = normalize_asset_name(filename, kind, &st);
        let path = format!("docs/bot/{bot_name}/{st}-{name}");
        let url = format!("memory://{path}");
        self.files
            .lock()
            .expect("assets lock")
            .push((url.clone(), bytes.to_vec()));
        Ok(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// normalize_asset_name зовётся store'ом: скрин с пробелами/кириллицей
    /// → сырой путь валиден, URL — percent-encoded (рядовой кейс #1171).
    #[test]
    fn raw_url_percent_encodes_spaces_and_cyrillic() {
        let name = normalize_asset_name(Some("скрин от МПНЧ.png"), MediaKind::Document, "1");
        assert_eq!(name, "скрин от МПНЧ.png");
        let url = raw_url("o/r", "main", &format!("docs/bot/x/1-{name}"));
        assert!(!url.contains(' '), "пробелы закодированы: {url}");
        assert!(url.contains("%20"), "пробел = %20: {url}");
        assert!(
            url.contains("%D0%BC") || url.contains("%D1%81"),
            "кириллица закодирована: {url}"
        );
        assert!(url.ends_with(".png"), "расширение живёт: {url}");
    }

    /// Путь ассета всегда внутри docs/bot/<бот>/, одно-компонентное
    /// имя — сепараторы из file_name не пробрасываются в путь (#1171).
    #[test]
    fn asset_path_is_single_component_name() {
        let name = normalize_asset_name(Some("../../etc/passwd"), MediaKind::Document, "1");
        assert!(!name.contains('/'), "сепаратор выжил: {name}");
    }

    #[tokio::test]
    async fn memory_store_roundtrip_url_and_bytes() {
        let store = MemoryAssetStore::default();
        let url = store
            .put("bot/Помощник", MediaKind::Photo, None, b"\x89PNG")
            .await
            .expect("put");
        // фолбэк имени фото с реальным stamp: <ms>-photo-<ms>.jpg
        let tail = url.rsplit('/').next().unwrap_or_default();
        assert!(
            tail.ends_with(".jpg") && tail.contains("-photo-"),
            "url: {tail}"
        );
        let head = tail.split('-').next().unwrap_or_default();
        assert!(
            !head.is_empty() && head.chars().all(|c| c.is_ascii_digit()),
            "url: {url}"
        );
        assert!(url.contains("docs/bot/Помощник/"), "url: {url}");
        // байты сохранены в store (приватно проверяемо внутри структуры)
        assert!(!store.files.lock().expect("l").is_empty());
    }
}
