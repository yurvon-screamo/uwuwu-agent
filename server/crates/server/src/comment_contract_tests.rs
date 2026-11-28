//! Контракт комментариев гарда semantic-sync (#1755).
//!
//! #1730 снял временно́й лимит тика semantic-sync (`timeout: None`), но в коде
//! остались комментарии «гард тика 120 с» — ревью #1746 перенесло их дословно
//! в UC-04 Спеки как действующий факт. Тест не даёт устаревшему утверждению
//! вернуться в исходники: иначе следующий синк доков снова унёс бы его
//! в документацию.

const SEARCH_MOD: &str = include_str!("search/mod.rs");
const SEARCH_SEMANTIC: &str = include_str!("search/semantic.rs");
const SEMANTIC_BACKEND: &str = include_str!("search/semantic_backend.rs");
const MAIN: &str = include_str!("main.rs");

/// Непрерывный блок комментарийных строк (`//` / `///`), содержащий `marker`.
fn comment_block(source: &str, marker: &str) -> String {
    let byte_pos = source
        .find(marker)
        .unwrap_or_else(|| panic!("маркер `{marker}` не найден в исходнике"));
    let lines: Vec<&str> = source.lines().collect();
    let idx = source[..byte_pos].lines().count().saturating_sub(1);
    let is_comment = |s: &str| s.trim_start().starts_with("//");
    let mut first = idx;
    while first > 0 && is_comment(lines[first - 1]) {
        first -= 1;
    }
    let mut last = idx;
    while last + 1 < lines.len() && is_comment(lines[last + 1]) {
        last += 1;
    }
    lines[first..=last].join("\n")
}

/// Снятый #1730 лимит не должен вернуться ни в один комментарий гарда.
fn assert_no_stale_limit(block: &str, where_: &str) {
    for stale in ["120 с", "120-секунд", "успевается за", "гард тика 120"]
    {
        assert!(
            !block.contains(stale),
            "{where_}: в комментарии гарда semantic-sync осталось снятое #1730 утверждение `{stale}`:\n{block}"
        );
    }
}

#[test]
fn semantic_sync_budget_doc_describes_capacity_guard_without_time_limit() {
    let block = comment_block(SEARCH_MOD, "pub const SEMANTIC_SYNC_BUDGET");
    assert_no_stale_limit(&block, "search/mod.rs SEMANTIC_SYNC_BUDGET");
    assert!(
        block.contains("емкостный гард"),
        "емкостная природа гарда не описана:\n{block}"
    );
    assert!(
        block.contains("#1730"),
        "ссылка на #1730 потеряна:\n{block}"
    );
    assert!(
        block.contains("SIDECAR_TIMEOUT"),
        "источник времени для остальных сайдкаров не указан:\n{block}"
    );
}

#[test]
fn semantic_sync_tick_comment_has_no_time_limit() {
    let block = comment_block(MAIN, "match b.sync_index(");
    assert_no_stale_limit(&block, "main.rs semantic-sync тик");
    assert!(
        block.contains("#1730"),
        "ссылка на #1730 потеряна:\n{block}"
    );
    assert!(
        block.contains("#1725"),
        "провенанс лимита 512 (#1725) потерян:\n{block}"
    );
    assert!(block.contains("512"), "ёмкость 512 не описана:\n{block}");
    assert!(
        block.contains("SIDECAR_TIMEOUT"),
        "остальные сайдкары не привязаны к SIDECAR_TIMEOUT:\n{block}"
    );
}

#[test]
fn client_timeouts_doc_does_not_claim_tick_guard() {
    let block = comment_block(SEARCH_SEMANTIC, "pub struct ClientTimeouts");
    assert_no_stale_limit(&block, "search/semantic.rs ClientTimeouts");
    assert!(
        block.contains("#1730"),
        "ссылка на #1730 потеряна:\n{block}"
    );
}

#[test]
fn sha_increment_comment_does_not_claim_sidecar_timeout_as_active() {
    let block = comment_block(SEMANTIC_BACKEND, "иначе сотни read_doc");
    assert!(
        !block.contains("таймаут сайдкара"),
        "semantic_backend.rs: снятый #1730 таймаут подан как действующее следствие:\n{block}"
    );
    assert!(
        block.contains("#1730"),
        "ссылка на #1730 потеряна:\n{block}"
    );
}
