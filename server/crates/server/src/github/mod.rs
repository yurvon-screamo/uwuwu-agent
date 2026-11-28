//! GitHub-адаптеры: доска/issues (hub), нативные поля (fields), PR-шлюз (prgate),
//! Pv2 REST-мутации статусов (pv2 — развод GraphQL/REST квот, #1236).

mod fields;
mod hub;
mod prgate;
mod pv2;
mod pv2_parse;
#[cfg(test)]
mod pv2_tests;

/// Жёсткий таймаут исходящих HTTP GitHub-контура (#1502): клиент без таймаута
/// при зависшем запросе (GitHub edge не отвечает) блокирует однопоточный
/// tick_loop навсегда — инцидент 22.11 (gql 502-деградация + вечное висение,
/// фабрика парализована). 120 c — выше максимального легитимного
/// наблюдаемого запроса (~40 c снапшот), ниже poll_sec (180 c).
pub(crate) fn http_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(uwuwu_usecase::runtime_cfg::github::http_timeout_secs())
}

pub use fields::{F_DEV_SESSION, F_REVIEW_SESSION, F_TEST_SESSION};
pub use hub::GitHubHub;
pub use prgate::{parse_pr_url, GhPrGateway};
