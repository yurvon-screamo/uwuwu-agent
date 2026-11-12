//! Аттачи задач: валидация путей, upload, percent-кодирование,
//! asset-ссылки markdown, скачивание ассетов тела.

use crate::cmd::sy_dispatch::guards::{looks_like_inline_text, not_a_path_error};
use crate::siyuan::SiYuan;
use anyhow::{Context, Result};

/// Файлы аттачей: существуют и это файлы — до любых сетевых вызовов.
/// Не-путь (вставленный текст вместо пути) — понятный отказ с именем флага.
pub(super) fn validate_attach_files(files: &[std::path::PathBuf]) -> Result<&[std::path::PathBuf]> {
    for file in files {
        let shown = file.to_string_lossy();
        if looks_like_inline_text(&shown) {
            return Err(not_a_path_error(
                "--attach",
                &shown,
                "укажи путь к существующему файлу",
            ));
        }
        let meta = std::fs::metadata(file)
            .with_context(|| format!("файл {}: недоступен", file.display()))?;
        anyhow::ensure!(meta.is_file(), "{} — не файл (директория?)", file.display());
    }
    Ok(files)
}

/// Расширения, которые SiYuan рендерит картинками; прочие аттачи — ссылки.
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "svg", "webp", "apng", "avif", "bmp", "ico",
];

/// Сегменты пути → байты вне RFC3986-unreserved → %XX (upper hex); `/`
/// остаётся литеральным разделителем. Симметрично экспорту ядра (пробелы
/// %20, кириллица %D0%B8… — фикстура collect_asset_refs) и его читателю:
/// префикс `](assets/` у читателя литеральный, `%2F` его разорвал бы
/// (поймано round-trip тестом), сырой пробел/`)` в имени режет ссылку.
pub(super) fn percent_encode_asset_path(path: &str) -> String {
    path.split('/')
        .map(percent_encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

pub(super) fn percent_encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn is_image_name(name: &str) -> bool {
    name.rsplit('.')
        .next()
        .map(|ext| ext.to_lowercase())
        .is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.as_str()))
}

/// Markdown-референс ассета в теле: картинка `![имя](assets/…)` или
/// ссылка `[имя](assets/…)`. Alt-текст без `[`/`]` — не рвать markdown.
pub(super) fn asset_ref_md(asset: &crate::siyuan::UploadedAsset) -> String {
    let alt = asset.original_name.replace(['[', ']'], "");
    let encoded = percent_encode_asset_path(&asset.asset_path);
    let prefix = if is_image_name(&asset.original_name) {
        "!"
    } else {
        ""
    };
    format!("{prefix}[{alt}]({encoded})")
}

/// Блок референсов для append-а в тело: ведущая пустая строка отделяет
/// от контента (шаблона/тега типа), по строке на ассет.
pub(super) fn attach_refs_md(assets: &[crate::siyuan::UploadedAsset]) -> String {
    let mut out = String::from("\n");
    for asset in assets {
        out.push_str(&asset_ref_md(asset));
        out.push('\n');
    }
    out
}

/// Asset-ссылки markdown-тела `](assets/…)` в порядке появления без дублей:
/// путь до `)` или пробела, percent-decode %XX (SiYuan кодирует пробелы и
/// юникод в именах аттачей). HTML-формы `<img src=…>` в экспорте не встречались.
pub(super) fn collect_asset_refs(md: &str) -> Vec<String> {
    const PREFIX: &str = "](assets/";
    let mut out = Vec::new();
    let mut rest = md;
    while let Some(pos) = rest.find(PREFIX) {
        rest = &rest[pos + PREFIX.len()..];
        let raw: String = rest
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != ')')
            .collect();
        rest = rest.strip_prefix(raw.as_str()).unwrap_or(rest);
        let key = percent_decode(&raw);
        if !key.is_empty() && !out.contains(&key) {
            out.push(key);
        }
    }
    out
}

/// %XX → байты (имена аттачей SiYuan percent-кодирует); невалидные
/// последовательности копируются как есть.
pub(super) fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Ключ asset-ссылки безопасен: ни один сегмент пути не пуст, «.» или «..» —
/// такие ключи («assets/../../etc») не скачиваются вовсе, до сетевого вызова.
pub(super) fn asset_key_is_safe(key: &str) -> bool {
    key.split('/')
        .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

/// Выгрузить asset-аттачи тела в /tmp/uwuwu-task-<row-id>/ (полный row-id —
/// контракт AGENTS.md; таймстамп-голова давала бы коллизии каталогов задач
/// одного дня). Оригинальные имена файлов; максимум 10; сбой одного аттача —
/// тихий пропуск (остаёмся на ссылках, как в аффин-контракте). Ключи с
/// точечными сегментами (../) не скачиваются вовсе.
pub(super) async fn save_body_assets(sy: &SiYuan, md: &str, row_id: &str) -> Vec<String> {
    const MAX_ASSETS: usize = 10;
    let refs = collect_asset_refs(md);
    let safe: Vec<&String> = refs.iter().filter(|key| asset_key_is_safe(key)).collect();
    if safe.is_empty() {
        return Vec::new();
    }
    let dir = format!("/tmp/uwuwu-task-{row_id}");
    if std::fs::create_dir_all(&dir).is_err() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for key in safe.into_iter().take(MAX_ASSETS) {
        let Ok(bytes) = sy.download_file(&format!("assets/{key}")).await else {
            continue;
        };
        // ядро молча отдаёт 200+0 байт для отсутствующего файла (контракт 3.8.9):
        // пустой ответ = файла нет, локальный пустыш не пишем
        if bytes.is_empty() {
            continue;
        }
        let name = key.rsplit('/').next().unwrap_or(key);
        let path = format!("{dir}/{name}");
        if std::fs::write(&path, &bytes).is_ok() {
            out.push(path);
        }
    }
    out
}
