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
