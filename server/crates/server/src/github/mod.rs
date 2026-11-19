//! GitHub-адаптеры: доска/issues (hub), нативные поля (fields), PR-шлюз (prgate),
//! Pv2 REST-мутации статусов (pv2 — развод GraphQL/REST квот, #1236).

mod fields;
mod hub;
mod prgate;
mod pv2;
mod pv2_parse;
#[cfg(test)]
mod pv2_tests;

pub use fields::{F_DEV_SESSION, F_REVIEW_SESSION, F_TEST_SESSION};
pub use hub::GitHubHub;
pub use prgate::{parse_pr_url, GhPrGateway};
