//! Интеграционные тесты `server/ops/task-env.sh` (subprocess-граница
//! bash+git). Скрипт — детерминированная механика env-ворктри фабрики
//! (ADR-041): worktree от origin-базы мастер-чекаута (base-резолв
//! origin/HEAD || origin/master || origin/main), ветка work/task-<row>,
//! схема ~/agents-env/<row>/{repo,scratch,logs}. #1333 не менял скрипт —
//! тесты защищают контракт, на который лег перевод носителей (В2/Т1):
//! env-ворктри несёт код носителя, а не каркас.
//! Фикстура — локальный bare-origin + клон-«носитель» во временных
//! каталогах: живых внешних ресурсов нет, CI-безопасно.

use std::path::{Path, PathBuf};
use std::process::Command;

fn script_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ops/task-env.sh")
}

fn git_out(dir: &Path, args: &[&str]) -> (String, String) {
    let out = Command::new("git")
        .arg("-c")
        .arg("user.email=test@local")
        .arg("-c")
        .arg("user.name=test")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git вызывается");
    assert!(
        out.status.success(),
        "git {args:?} упал: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

struct Fixture {
    root: PathBuf,
    carrier: PathBuf,
}

/// Носитель кода: bare-origin + клон с маркерным коммитом на master.
fn fixture() -> Fixture {
    let uniq = format!(
        "uwuwu-task-env-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("часы идут")
            .as_nanos()
    );
    let root = std::env::temp_dir().join(uniq);
    std::fs::create_dir_all(&root).expect("tmp корень создан");
    let bare = root.join("origin.git");
    git_out(
        &root,
        &[
            "init",
            "--bare",
            "-b",
            "master",
            bare.to_str().expect("utf8"),
        ],
    );
    let carrier = root.join("carrier");
    git_out(
        &root,
        &[
            "clone",
            "-q",
            bare.to_str().expect("utf8"),
            carrier.to_str().expect("utf8"),
        ],
    );
    std::fs::write(
        carrier.join("carrier-marker.txt"),
        "real code carrier (#1333 probe fixture)\n",
    )
    .expect("маркер записан");
    git_out(&carrier, &["add", "carrier-marker.txt"]);
    git_out(&carrier, &["commit", "-q", "-m", "carrier marker"]);
    git_out(&carrier, &["push", "-q", "origin", "master"]);
    git_out(&carrier, &["fetch", "-q", "origin"]);
    Fixture { root, carrier }
}

fn run_task_env(env_root: &Path, cwd: &Path, row: &str) -> std::process::Output {
    Command::new("bash")
        .arg(script_path())
        .arg(row)
        .env("UWUWU_ENV_ROOT", env_root)
        .current_dir(cwd)
        .output()
        .expect("bash вызывается")
}

fn cleanup(fx: Fixture) {
    let _ = std::fs::remove_dir_all(&fx.root);
}

#[test]
fn task_env_creates_worktree_carrying_carrier_code() {
    let fx = fixture();
    let env_root = fx.root.join("env");
    let out = run_task_env(&env_root, &fx.carrier, "1333fx");
    assert!(
        out.status.success(),
        "скрипт зелёный: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let env_dir = env_root.join("1333fx");
    let repo = env_dir.join("repo");
    assert!(
        repo.join(".git").exists(),
        "env repo создан: {}",
        repo.display()
    );
    let (head, _) = git_out(&repo, &["rev-parse", "HEAD"]);
    let (base, _) = git_out(&fx.carrier, &["rev-parse", "origin/master"]);
    assert_eq!(
        head.trim(),
        base.trim(),
        "env-ворктри от origin-базы носителя"
    );
    let (branch, _) = git_out(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(branch.trim(), "work/task-1333fx", "ветка work/task-<row>");
    assert!(
        repo.join("carrier-marker.txt").exists(),
        "env несёт код носителя (маркер читается в ворктри)"
    );
    assert!(
        env_dir.join("scratch").is_dir() && env_dir.join("logs").is_dir(),
        "схема канона: repo/scratch/logs"
    );
    cleanup(fx);
}

#[test]
fn task_env_rejects_empty_row_loudly() {
    let fx = fixture();
    let out = run_task_env(&fx.root.join("env"), &fx.carrier, "");
    assert!(
        !out.status.success(),
        "пустой row-id — громкий отказ, не тихое создание"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("usage: task-env"),
        "usage-подсказка в stderr: {stderr}"
    );
    cleanup(fx);
}
