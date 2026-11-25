//! Слаг-гард write-путей вики (#1613): запись по неполному слагу не
//! создаёт файл вне структуры. Ветки отказа: root-prefix (корень вики —
//! закрытый набор папок; одно-сегментный таргет — файл в корне),
//! parent-collision (родитель — живой файл и не папка), suffix-collision
//! (живой док с совпавшим путём-хвостом). Чистый предикат по живому
//! дереву слагов; проводка — тул-слой docs_write/docs_append. Родня
//! fm_guard: чистые функции в usecase, честное имя ветки в ответе тула.

/// Корень вики — закрытый набор (contents API, сверка 25.11.2026).
/// Динамический вывод первых сегментов дерева отвергнут: один паразит
/// легализует семью; новая корневая папка — осознанная правка константы.
pub const ROOT_FOLDERS: [&str; 5] = ["Experience", "Projects", "bot", "cron", "person"];

/// Вердикт предиката — ветка отказа с кандидатами или чисто.
#[derive(Debug, PartialEq, Eq)]
pub enum SlugVerdict {
    /// Коллизий нет — прежнее поведение записи.
    Clear,
    /// Одно-сегментный таргет (файл в корне вики) либо первый сегмент
    /// вне ROOT_FOLDERS. Кандидаты — живые доки с совпавшим хвостом
    /// (класс «совпавший тайтл» #1219).
    RootPrefix(Vec<String>),
    /// Родительский путь — живой файл и при этом не папка (детей нет).
    /// Легаси-стем «файл + папка» записывать под собой разрешает.
    ParentCollision(String),
    /// Живой док, чей путь строго длиннее с границей по слешу и хвостом
    /// равным таргету (вырожденный класс вложенного корнеимени).
    SuffixCollision(Vec<String>),
}

/// Живые доки, чей путь — строго более длинный с границей по слешу, чей
/// хвост равен таргету. Единый источник кандидатов: ветка RootPrefix
/// предиката и append-тул при AppendMissing.
pub fn suffix_candidates(target: &str, live: &[String]) -> Vec<String> {
    let tail = format!("/{target}");
    live.iter()
        .filter(|l| l.len() > target.len() && l.ends_with(&tail))
        .cloned()
        .collect()
}

/// Ветвление: root → parent → suffix → clear. Точное совпадение таргета
/// с элементом дерева ни в одну ветку не попадает (суффикс — строго
/// длиннее, родитель — другой путь): противоречие устаревшего дерева с
/// авторитетным чтением разрешается на проводке (read-first).
pub fn check(target: &str, live: &[String]) -> SlugVerdict {
    if !target.contains('/') || !ROOT_FOLDERS.contains(&root_segment(target)) {
        return SlugVerdict::RootPrefix(suffix_candidates(target, live));
    }
    // parent-collision по всем префиксам пути: живой файл без детей на
    // любом уровне делает запись под собой невозможной (файл ≠ папка) —
    // иначе глубокий путь рождает папку рядом с файлом, новую пару
    // «файл + папка» того же класса
    let segments: Vec<&str> = target.split('/').collect();
    for depth in 1..segments.len() {
        let parent = segments[..depth].join("/");
        if !live.iter().any(|l| l == &parent) {
            continue;
        }
        let prefix = format!("{parent}/");
        if !live.iter().any(|l| l.starts_with(&prefix)) {
            return SlugVerdict::ParentCollision(parent);
        }
    }
    let candidates = suffix_candidates(target, live);
    if candidates.is_empty() {
        SlugVerdict::Clear
    } else {
        SlugVerdict::SuffixCollision(candidates)
    }
}

/// Root-ветка без дерева: одно-сегментный таргет (файл в корне вики) или
/// корень вне канона. Доступна при сбое чтения (существование неизвестно)
/// — ни от чтения, ни от дерева не зависит; кандидатов в этом окне нет.
/// None — root-проверка пройдена.
pub fn check_root(target: &str) -> Option<Vec<String>> {
    if target.contains('/') && ROOT_FOLDERS.contains(&root_segment(target)) {
        return None;
    }
    Some(Vec::new())
}

fn root_segment(target: &str) -> &str {
    target.split('/').next().unwrap_or_default()
}
