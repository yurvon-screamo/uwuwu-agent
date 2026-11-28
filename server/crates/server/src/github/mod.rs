//! GitHub-адаптеры: доска/issues (hub), нативные поля (fields), PR-шлюз (prgate),
//! Pv2 REST-мутации статусов (pv2 — развод GraphQL/REST квот, #1236).

mod fields;
mod hub;
mod prgate;
mod pv2;
mod pv2_parse;
#[cfg(test)]
mod pv2_tests;

/// Жёсткий таймаут HTTP-клиентов контура (#1502): клиент без таймаута при
/// зависшем запросе блокирует однопоточный tick_loop. Значение — из
/// `[github] http_timeout_secs` config.toml; положительность проверяется
/// на старте (`Config::validate_runtime`).
pub(crate) fn http_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(uwuwu_usecase::runtime_cfg::github::http_timeout_secs())
}

pub use fields::{F_DEV_SESSION, F_REVIEW_SESSION, F_TEST_SESSION};
pub use hub::GitHubHub;
pub use prgate::{parse_pr_url, GhPrGateway};
