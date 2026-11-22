//! Зеркало src/search для интеграционных тестов (крейт бинарный, lib-таргета
//! нет): `crate::search::…` внутри подключённых модулей резолвится, пути
//! `#[path]` — от директории этого mod-rs файла (tests/search/).

// Зеркало тянет клиент Gemini целиком — интеграционные пины юзают только
// трейт/бэкенд, непотребленные символы клиента — не запах теста.
#[allow(dead_code)]
#[path = "../../src/search/semantic.rs"]
pub mod semantic;
#[path = "../../src/search/semantic_backend.rs"]
pub mod semantic_backend;

#[path = "../../src/search/budget.rs"]
pub mod budget;

#[path = "../../src/search/maintenance.rs"]
pub mod maintenance;

#[path = "dim_migration.rs"]
pub mod dim_migration;

#[path = "churn_maintenance.rs"]
pub mod churn_maintenance;

#[path = "gemini_live.rs"]
pub mod gemini_live;

#[path = "support.rs"]
#[allow(dead_code)]
pub mod support;
