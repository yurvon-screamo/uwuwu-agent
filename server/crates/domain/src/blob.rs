//! Ключ объекта blob-канала (#1292): валидный путь в приватном S3-бакете.

/// Ключ объекта `blobs/<project>/<task|misc>/<sha8>-<filename>`.
/// Гарды: префикс `blobs/`, сегменты непустые и без `.`/`..`, без
/// обратных слэшей и управляющих символов. Пробелы и кириллица в имени
/// файла легальны — транспорты percent-encode путь сами.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobKey(String);

impl BlobKey {
    /// Валидирующий парсер: `blobs/…` или None.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        let rest = raw.strip_prefix("blobs/")?;
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
