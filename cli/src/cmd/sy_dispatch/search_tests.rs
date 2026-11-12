//! Тесты search: байт-стабильный контракт формата search_head.

use super::search::search_head;

/// Golden stdout-головы search (Low ревью PR #78): формат парсят
/// фабричные агенты — правка println! мимо search_head невозможна,
/// правка самого search_head красит этот тест.
#[test]
fn search_head_is_byte_stable_contract_format() {
    assert_eq!(
        search_head("семантический индекс"),
        "Searching for: \"семантический индекс\""
    );
}

// ——— html_embed: append-хирургия и rewrite-инлайн ———
