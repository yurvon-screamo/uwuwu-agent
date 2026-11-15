//! GitHub-адаптеры: доска/issues (hub), нативные поля (fields), PR-шлюз (prgate).

mod fields;
mod hub;
mod prgate;

pub use fields::{F_DEV_SESSION, F_REVIEW_SESSION, F_TEST_SESSION};
pub use hub::GitHubHub;
pub use prgate::GhPrGateway;
