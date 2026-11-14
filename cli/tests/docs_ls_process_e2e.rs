//! E2E docs ls на уровне процесса (задача 1094, стадия тестирования):
//! живая команда `docs ls` против изолированного file://-рига через
//! UWUWU_HUB_CLONE_ROOT — тот сценарий, что прогоняется руками
//! (`uwuwu-cli docs ls "nosuchplace123"` на живом клоне hub), только
//! без сети (origin — локальный bare).
//!
//! Почему уровень процесса: контракт задачи — то, что видит юзер живой
//! команды (rc, stderr-отказ с раскладкой вместо сырой os error,
//! stdout-перечень детей) — складывается из CLI-парсинга + cmd-слоя
//! (hub_docs::docs_ls) + store; юниты store (github_doc_clone_tests)
//! stderr живого процесса не видят (прецедент — clone_process_e2e.rs).
//!
//! Пины:
//! A. Главный сценарий (репро из постановки): `docs ls "nosuchplace123"`
//!    — rc 1, stderr — отказ с раскладкой детей docs/, без «os error».
//! B. Хеппи-путь живой командой: `docs ls "Projects"` — rc 0, stdout —
//!    дети, формат прежний (папки с «/»).

use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_uwuwu-cli")
}

fn git(args: &[&str]) {
    let out = Command::new("git").args(args).output().expect("git spawn");
    assert!(
        out.status.success(),
        "git {args:?} упал: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Риг формы cli: bare origin + клон в каталоге, куда cli смотрит через
/// UWUWU_HUB_CLONE_ROOT. Seed — три топ-папки + два проекта (раскладка
/// верхнего уровня и живой happy-path). Возвращает (tmp, clone_root, clone).
fn setup_rig(name: &str) -> (TempDir, PathBuf, PathBuf) {
    let tmp = TempDir::new().expect("tmpdir");
    let root = tmp.path();

    let origin = root.join(format!("{name}-origin.git"));
    git(&[
        "init",
        "--bare",
        "-q",
        "-b",
        "main",
        origin.to_str().unwrap(),
    ]);

    let writer = root.join(format!("{name}-writer"));
    git(&["init", "-q", "-b", "main", writer.to_str().unwrap()]);
    for rel in [
        "docs/Experience/person/PERSON.md",
        "docs/Projects/uwuwu/README.md",
        "docs/Projects/origa/README.md",
        "docs/Daily/2026-11-14.md",
    ] {
        let f = writer.join(rel);
        std::fs::create_dir_all(f.parent().unwrap()).expect("seed dir");
        std::fs::write(&f, "# seed\n").expect("seed file");
    }
    git(&["-C", writer.to_str().unwrap(), "add", "-A"]);
    git(&[
        "-C",
        writer.to_str().unwrap(),
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-q",
        "-m",
        "seed",
    ]);
    git(&[
        "-C",
        writer.to_str().unwrap(),
        "push",
        "-q",
        origin.to_str().unwrap(),
        "main",
    ]);

    // клон, который найдёт cli: <clone-root>/local__test
    let clone_root = root.join("hub-clone");
    let clone = clone_root.join("local__test");
    std::fs::create_dir_all(&clone).expect("clone dir");
    git(&["init", "-q", "-b", "main", clone.to_str().unwrap()]);
    git(&[
        "-C",
        clone.to_str().unwrap(),
        "remote",
        "add",
        "origin",
        origin.to_str().unwrap(),
    ]);
    git(&[
        "-C",
        clone.to_str().unwrap(),
        "fetch",
        "-q",
        "--depth",
        "1",
        "origin",
    ]);
    git(&[
        "-C",
        clone.to_str().unwrap(),
        "checkout",
        "-qf",
        "-B",
        "main",
        "origin/main",
    ]);

    let clone_root = clone.parent().unwrap().to_path_buf();
    (tmp, clone_root, clone)
}

/// Живой вызов cli: `docs ls <prefix>` с env, изолирующим клон-каталог.
fn cli_docs_ls(clone_root: &Path, prefix: &str) -> (Option<i32>, String, String) {
    let out = Command::new(bin())
        .arg("docs")
        .arg("ls")
        .arg(prefix)
        .env("UWUWU_HUB_CLONE_ROOT", clone_root)
        .env("UWUWU_HUB_REPO", "local/test")
        .output()
        .expect("cli spawn");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A. Главный сценарий (репро из постановки, 14.11): неизвестное
/// пространство — отказ с раскладкой детей docs/ в stderr, rc 1,
/// сырой os error не выдаётся.
#[test]
fn docs_ls_unknown_path_hints_layout_instead_of_raw_os_error() {
    let (_tmp, clone_root, _clone) = setup_rig("ls-e2e-unknown");
    let (code, stdout, stderr) = cli_docs_ls(&clone_root, "nosuchplace123");
    assert_eq!(code, Some(1), "unknown-путь должен отказывать: {stdout}");
    assert!(
        stderr.contains("не найден в docs/"),
        "отказ без «не найден в docs/»: {stderr}"
    );
    assert!(
        stderr.contains("доступно в «docs»: Daily/, Experience/, Projects/"),
        "раскладка топ-папок не в отказе: {stderr}"
    );
    assert!(
        !stderr.contains("os error"),
        "сырая os error просочилась в отказ: {stderr}"
    );
    assert!(
        stdout.is_empty(),
        "при отказе stdout должен быть пуст: {stdout}"
    );
}

/// B. Хеппи-путь живой командой: известная папка — дети в stdout, rc 0,
/// формат прежний (папки с «/», сортировка по имени).
#[test]
fn docs_ls_known_folder_lists_children() {
    let (_tmp, clone_root, _clone) = setup_rig("ls-e2e-happy");
    let (code, stdout, stderr) = cli_docs_ls(&clone_root, "Projects");
    assert_eq!(code, Some(0), "happy-path упал: {stderr}");
    assert_eq!(
        stdout, "origa/\nuwuwu/\n",
        "перечень детей Projects искажён: {stdout}"
    );
}
