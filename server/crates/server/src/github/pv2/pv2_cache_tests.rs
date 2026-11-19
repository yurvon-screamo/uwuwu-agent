//! Юнит-пины кеш-щита read_status (#1346, ревизия 4): write-through
//! статуса прикрывает стейл items-индекса в своём TTL-окне — инвариант
//! «STATUS_TTL >= ITEMS_TTL» (items-карта доски меняется wholesale при
//! любом рефетче, до-записное значение нашей строки возвращается
//! лаг-фетчем). Фикстуры без HTTP (ADR-110): кеши Pv2Rest наполняются
//! напрямую, чтение остаётся на живой items-карте (fetch_items не
//! вызывается).

use super::{Pv2Item, Pv2Rest, ITEMS_TTL, STATUS_TTL};
use std::collections::HashMap;
use std::time::Duration;

/// Arrange-фикстура: items-карта доски со статусом носителя (кладётся
/// свежей — возраст 0), wt записи — опционально с возрастом ago.
fn rest_with(board: &str, number: u64, item_status: &str, wt: Option<(&str, Duration)>) -> Pv2Rest {
    let rest = Pv2Rest::new("o/r".into(), "t".into());
    let now = std::time::Instant::now();
    let mut map = HashMap::new();
    map.insert(
        number,
        Pv2Item {
            id: 1,
            status: Some(item_status.to_string()),
        },
    );
    rest.items.put(board, map, now);
    if let Some((name, ago)) = wt {
        rest.status_wt
            .put(&format!("{board}#{number}"), name.to_string(), now - ago);
    }
    rest
}

/// Контракт #1346: гард-чтение в TTL-окне wt не видит стейл items-карты.
/// wt, положенный 100 с назад, при TTL 60 (мастер) истёк — красный;
/// при TTL 240 (фикс) жив — зелёный.
#[tokio::test]
async fn read_status_shields_stale_item_index_within_status_ttl() {
    // Arrange: запись In Progress 100 с назад, items-карта несёт стейл Analys
    let rest = rest_with(
        "B",
        7,
        "Analys",
        Some(("In Progress", Duration::from_secs(100))),
    );
    // Act: гард-чтение current
    let raw = rest.read_status("B", 7).await.unwrap();
    // Assert: щит wt — до-записное значение карты не просачивается
    assert_eq!(
        raw.as_deref(),
        Some("In Progress"),
        "wt прикрывает стейл items-карты в своём TTL-окне"
    );
}

/// Граница щита: ровно STATUS_TTL — строгий `<` выключает щит, отвечает
/// носитель (items-карта, стейл). Пин против нечаянного `<=`.
#[tokio::test]
async fn status_ttl_shield_exactly_at_expiry_serves_carrier() {
    let rest = rest_with("B", 7, "Analys", Some(("In Progress", STATUS_TTL)));
    let raw = rest.read_status("B", 7).await.unwrap();
    assert_eq!(
        raw.as_deref(),
        Some("Analys"),
        "строгое < ttl: на границе щит снят"
    );
}

/// Граница щита: за 1 с до STATUS_TTL wt ещё прикрывает стейл карты.
#[tokio::test]
async fn status_ttl_shield_one_sec_before_expiry_still_shields() {
    let rest = rest_with(
        "B",
        7,
        "Analys",
        Some(("In Progress", STATUS_TTL - Duration::from_secs(1))),
    );
    let raw = rest.read_status("B", 7).await.unwrap();
    assert_eq!(raw.as_deref(), Some("In Progress"), "щит жив до границы");
}

/// wt-first: при живом wt гард-чтение не доходит до items-карты —
/// маркер-стейл карты не просачивается (пин против регрессии порядка).
#[tokio::test]
async fn live_wt_serves_guard_without_touching_item_index() {
    let rest = rest_with(
        "B",
        7,
        "Analys",
        Some(("In Progress", Duration::from_secs(10))),
    );
    let raw = rest.read_status("B", 7).await.unwrap();
    assert_eq!(
        raw.as_deref(),
        Some("In Progress"),
        "wt-first: карта не читается"
    );
}

/// Пин честности verify (#1346 High-1 р1–р2): контрольное чтение
/// verify_status идёт через fetch_status — мимо wt. items-стейл не
/// подтверждает собственную запись (Mismatch — безопасное направление).
#[tokio::test]
async fn verify_control_read_sees_carrier_not_own_wt() {
    let rest = rest_with(
        "B",
        7,
        "Analys",
        Some(("In Progress", Duration::from_secs(10))),
    );
    let raw = rest.fetch_status("B", 7).await.unwrap();
    assert_eq!(
        raw.as_deref(),
        Some("Analys"),
        "fetch_status мимо wt — verify не самоподтверждается"
    );
}

/// Инвариант #1346: wt прикрывает максимум возраста items-карты
/// (wholesale-замена при любом рефетче). Дрейф STATUS_TTL ниже
/// ITEMS_TTL тихо открывает стейл-окно гарда.
#[test]
fn status_ttl_covers_items_index_max_age() {
    assert!(
        STATUS_TTL >= ITEMS_TTL,
        "STATUS_TTL >= ITEMS_TTL — инвариант #1346: лаг-рефетч карты кладёт \
         до-записный статус, за пределами wt гард увидит стейл"
    );
}
