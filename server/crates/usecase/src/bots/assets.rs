//! Нормализация имён ассетов бота — чистый контракт `BotAssetStore`
//! (#1171): из сырого file_name юзера строится безопасный одно-компонент
//! имени файла: путевые сепараторы и управляющие символы нейтрализуются,
//! вырожденные имена (`.`/`..`/пустое) → фолбэк по типу, длина ограничена
//! (компонент >255 байт валит git-пути и checkout'ы).

use super::ports::MediaKind;

/// Потолок имени в байтах: запас до 255-байтового лимита файловых систем
/// с прицелом на префикс `docs/bot/<бот>/<stamp>-`.
const NAME_MAX_BYTES: usize = 120;

/// Нормализованное имя файла ассета: One component, safe bytes, non-empty.
pub fn normalize_asset_name(raw: Option<&str>, kind: MediaKind, stamp: &str) -> String {
    let cleaned: Option<String> = raw
        .map(|n| {
            n.chars()
                .map(|c| {
                    if c == '/' || c == '\\' || c.is_control() {
                        '_'
                    } else {
                        c
                    }
                })
                .collect::<String>()
                .trim()
                .to_string()
        })
        .filter(|n| !n.is_empty() && n != "." && n != "..")
        .map(|n| truncate_utf8(&n, NAME_MAX_BYTES));
    match cleaned {
        Some(n) => n,
        None => match kind {
            MediaKind::Photo => format!("photo-{stamp}.jpg"),
            MediaKind::Document | MediaKind::Other => format!("file-{stamp}.bin"),
        },
    }
}

/// Обрезка по границе UTF-8 символа (не байтом в середину кодпоинта).
fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_name_passes_through() {
        assert_eq!(
            normalize_asset_name(Some("скрин.png"), MediaKind::Document, "111"),
            "скрин.png"
        );
    }

    #[test]
    fn path_separators_flatten_to_single_component() {
        // гвард path traversal: сепараторы → '_', компоненты сплющены
        assert_eq!(
            normalize_asset_name(Some("../../etc/passwd"), MediaKind::Document, "1"),
            ".._.._etc_passwd"
        );
        assert_eq!(
            normalize_asset_name(Some("a\\b\\c.png"), MediaKind::Document, "1"),
            "a_b_c.png"
        );
    }

    #[test]
    fn degenerate_names_fall_back_by_kind() {
        assert_eq!(
            normalize_asset_name(Some(".."), MediaKind::Photo, "111"),
            "photo-111.jpg"
        );
        assert_eq!(
            normalize_asset_name(Some("  "), MediaKind::Document, "222"),
            "file-222.bin"
        );
        assert_eq!(
            normalize_asset_name(None, MediaKind::Photo, "333"),
            "photo-333.jpg"
        );
    }

    #[test]
    fn control_chars_neutralized_and_trimmed() {
        assert_eq!(
            normalize_asset_name(Some("\u{7}x\u{1f}y.png"), MediaKind::Document, "1"),
            "_x_y.png"
        );
    }

    #[test]
    fn long_name_truncated_on_char_boundary() {
        let long = "я".repeat(200); // 400 байт
        let out = normalize_asset_name(Some(&long), MediaKind::Document, "1");
        assert!(out.len() <= 120, "len={}", out.len());
        // граница символа: строка валидна
        assert_eq!(out.chars().count(), 60);
    }
}
