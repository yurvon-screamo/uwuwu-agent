//! Ключ объекта blob-канала (#1292): валидный путь в приватном S3-бакете.

/// Ключ объекта blob-канала. Канон записи:
/// `blobs/<project>/<task|misc>/<id>-<filename>`; `<id>` — sha8 байтов для
/// data_base64 и UUID для резервирования presigned PUT. Legacy `assets/…`
/// (вложения, перенесённые из репо hub в бакет, #1365) легальны только на
/// чтении — `parse_read`. Гарды: сегменты непустые и без `.`/`..`, без
/// обратных слэшей и управляющих символов. Пробелы и кириллица в имени
/// файла легальны — транспорты percent-encode путь сами.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobKey(String);

impl BlobKey {
    /// Валидирующий парсер канона: `blobs/…` или None.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        Self::guarded(raw, raw.strip_prefix("blobs/")?)
    }

    /// Парсер чтения: канон `blobs/…` или legacy `assets/…`. Односторонний:
    /// канон строят `place()` и `reserve()`; в пресайн-режиме объект пишет
    /// агент напрямую, сервер только подписывает ключ.
    pub fn parse_read(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        let rest = raw
            .strip_prefix("blobs/")
            .or_else(|| raw.strip_prefix("assets/"))?;
        Self::guarded(raw, rest)
    }

    /// Общие гарды пути: без обратных слэшей и управляющих символов, без
    /// хвостового слэша; сегменты непустые и без `.`/`..`.
    fn guarded(raw: &str, rest: &str) -> Option<Self> {
        let safe = !raw.contains('\\')
            && !raw.ends_with('/')
            && !raw.chars().any(char::is_control)
            && rest
                .split('/')
                .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
        safe.then(|| Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_key_parses_with_cyrillic_and_spaces() {
        let key = BlobKey::parse("blobs/uwuwu/1292/a1b2c3d4-скрин от МПНЧ.png");
        assert!(key.is_some(), "канонический ключ валиден");
        assert!(BlobKey::parse("blobs/uwuwu/misc/a1b2c3d4-x.png").is_some());
    }

    #[test]
    fn read_parser_accepts_legacy_assets_and_canonical_keys() {
        assert!(
            BlobKey::parse_read("assets/task-1325/dash-cam-buttons.png").is_some(),
            "legacy-ключ перенесённого вложения читается"
        );
        assert!(
            BlobKey::parse_read("blobs/uwuwu/1292/a1b2c3d4-x.png").is_some(),
            "канон читается тем же парсером"
        );
    }

    #[test]
    fn legacy_prefix_is_read_only() {
        assert!(
            BlobKey::parse("assets/task-1325/x.png").is_none(),
            "канон-парсер put-пути legacy не принимает"
        );
        assert!(
            BlobKey::parse_read("blobsa/x").is_none(),
            "чужой префикс мимо обоих префиксов"
        );
    }

    #[test]
    fn read_parser_rejects_junk_paths() {
        assert!(BlobKey::parse_read("assets/../etc").is_none(), "traversal");
        assert!(BlobKey::parse_read("assets//x").is_none(), "пустой сегмент");
        assert!(BlobKey::parse_read("assets/").is_none(), "хвостовой слэш");
        assert!(
            BlobKey::parse_read("assets/a\\b").is_none(),
            "обратный слэш"
        );
        assert!(
            BlobKey::parse_read("assets/a\tb").is_none(),
            "control-символ"
        );
        assert!(BlobKey::parse_read("assets/./x").is_none(), "точка-сегмент");
    }

    #[test]
    fn traversal_and_junk_paths_are_rejected() {
        assert!(BlobKey::parse("blobs/uwuwu/../etc").is_none(), "traversal");
        assert!(BlobKey::parse("/etc/passwd").is_none(), "чужой префикс");
        assert!(
            BlobKey::parse("misc/a1b2c3d4-x.png").is_none(),
            "нет blobs/"
        );
        assert!(BlobKey::parse("blobs//x").is_none(), "пустой сегмент");
        assert!(BlobKey::parse("blobs/uwuwu/").is_none(), "хвостовой слэш");
        assert!(BlobKey::parse("blobs/a\\b").is_none(), "обратный слэш");
        assert!(BlobKey::parse("blobs/a\tb").is_none(), "control-символ");
        assert!(BlobKey::parse("blobs/./x").is_none(), "точка-сегмент");
    }
}
