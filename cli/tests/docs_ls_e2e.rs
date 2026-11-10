//! E2E-пин контракта `docs ls` на живом ядре SiYuan: перечень проектов через
//! `docs ls "Projects"` — путь, на который ведут help `projects`, методичка
//! дримера (agents/dreamer.md) и методичка ночного аудита (баг-пин задачи
//! 20261109132340-nje6zb4). До фикса вызов отказывает (пространства
//! «Projects» не существует: это имя ноутбука, в hpath не живёт) — тест
//! красный; после фикса — зелёный. Контроль: резолв известного проекта
//! («uwuwu») жив.
//!
//! Запуск: UWUWU_E2E=1 cargo test (без живого ядра тесты сразу зелёные).
//! Спавн собранного бинаря через CARGO_BIN_EXE — контракт проверяется ровно
//! как его видит пользователь: exit-код и stdout, а не внутренний API.

use std::process::Command;

fn e2e_enabled() -> bool {
    std::env::var("UWUWU_E2E").as_deref() == Ok("1")
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_uwuwu-cli")
}

fn docs_ls(prefix: &str) -> (Option<i32>, String, String) {
    let out = Command::new(bin())
        .args(["docs", "ls", prefix])
        .output()
        .expect("spawn uwuwu-cli");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Перечень проектов через штатный путь из help `projects`: exit 0,
/// непустой stdout, без «Error:» — вызов должен работать как заявлено
/// в интерфейсе, а не доставать перечень из текста отказа.
#[test]
fn projects_listing_via_docs_ls_projects() {
    if !e2e_enabled() {
        return;
    }
    let (code, stdout, stderr) = docs_ls("Projects");
    assert_eq!(code, Some(0), "docs ls \"Projects\" должен завершиться успехом (help projects указывает этот путь как штатный перечень проектов); stderr: {stderr}");
    assert!(
        !stderr.contains("Error"),
        "перечень не должен приходить в тексте ошибки; stderr: {stderr}"
    );
    assert!(
        !stdout.trim().is_empty(),
        "перечень проектов должен приходить в stdout, получено: {stdout:?}"
    );
}

/// Контроль резолва: известный проект листится штатно (граница жива —
/// если этот тест красный, сломано ядро/индекс, а не резолв «Projects»).
#[test]
fn known_project_listing_via_docs_ls() {
    if !e2e_enabled() {
        return;
    }
    let (code, stdout, stderr) = docs_ls("uwuwu");
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(
        !stdout.trim().is_empty(),
        "проект uwuwu не пуст — резолв сломан: {stdout:?}"
    );
}
