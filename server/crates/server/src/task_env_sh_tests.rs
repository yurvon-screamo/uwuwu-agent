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
    // Корень теста выдаётся атомарно: create_dir отказывает на занятом имени,
    // поэтому совпадение меток у параллельных тестов не даёт общего корня.
    let base = std::env::temp_dir();
    let pid = std::process::id();
    let root = loop {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("часы идут")
            .as_nanos();
        let candidate = base.join(format!("uwuwu-task-env-test-{pid}-{nanos}"));
        match std::fs::create_dir(&candidate) {
            Ok(()) => break candidate,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => panic!("tmp корень не создан: {err}"),
        }
    };
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
fn task_env_reuses_existing_worktree_without_touching_working_state() {
    let fx = fixture();
    let env_root = fx.root.join("env");
    let first = run_task_env(&env_root, &fx.carrier, "1504fx");
    let stderr_first = String::from_utf8_lossy(&first.stderr);
    assert!(
        first.status.success(),
        "первый вызов создал env: stderr={stderr_first}"
    );
    let repo = env_root.join("1504fx").join("repo");
    std::fs::write(repo.join("local-edit.txt"), "working state\n").expect("правка записана");
    let (head_before, _) = git_out(&repo, &["rev-parse", "HEAD"]);

    let second = run_task_env(&env_root, &fx.carrier, "1504fx");

    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        second.status.success(),
        "повторный вызов по живому env зелёный: stderr={stderr}"
    );
    assert!(!stderr.contains("fatal"), "fatal в stderr: {stderr}");
    assert_eq!(
        std::fs::read_to_string(repo.join("local-edit.txt")).expect("правка читается"),
        "working state\n",
        "рабочее состояние ворктри не тронуто"
    );
    let (head_after, _) = git_out(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(head_after, head_before, "HEAD ворктри не сдвинулся");
    cleanup(fx);
}

#[test]
fn task_env_reuses_classic_checkout_with_git_directory() {
    let fx = fixture();
    let env_root = fx.root.join("env");
    let repo = env_root.join("1504fx").join("repo");
    std::fs::create_dir_all(repo.parent().expect("env-каталог")).expect("env-каталог создан");
    let bare = fx.root.join("origin.git");
    git_out(
        &fx.root,
        &[
            "clone",
            "-q",
            bare.to_str().expect("utf8"),
            repo.to_str().expect("utf8"),
        ],
    );
    let (head_before, _) = git_out(&repo, &["rev-parse", "HEAD"]);

    let out = run_task_env(&env_root, &fx.carrier, "1504fx");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "классический чекаут с .git-каталогом переиспользуется: stderr={stderr}"
    );
    let (head_after, _) = git_out(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(head_after, head_before, "HEAD чекаута не сдвинулся");
    cleanup(fx);
}

#[test]
fn task_env_does_not_report_ready_over_stray_git_marker() {
    // Гард re-use требует не только наличия .git, но и настоящего git-носителя:
    // указатель на несуществующий gitdir не должен молча считаться живым env.
    // Оракул — код возврата, а не формулировка stdout.
    let fx = fixture();
    let env_root = fx.root.join("env");
    let repo = env_root.join("1504fx").join("repo");
    std::fs::create_dir_all(&repo).expect("repo-каталог создан");
    let gone = fx.root.join("gone-gitdir");
    std::fs::write(repo.join(".git"), format!("gitdir: {}\n", gone.display()))
        .expect("stray .git записан");

    let out = run_task_env(&env_root, &fx.carrier, "1504fx");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "сломанный .git не считается живым env: stdout={stdout} stderr={stderr}"
    );
    cleanup(fx);
}

#[test]
fn task_env_creates_worktree_for_empty_dir_inside_foreign_git_tree() {
    // Первая половина гарда: пустой repo без .git внутри чужого git-дерева —
    // не env. Без проверки [ -e "$repo/.git" ] git нашёл бы родителя и
    // ушёл в re-use, не создав ворктри.
    let fx = fixture();
    let env_root = fx.carrier.join("env-inside");
    let repo = env_root.join("1504fx").join("repo");
    std::fs::create_dir_all(&repo).expect("пустой repo-каталог создан");

    let out = run_task_env(&env_root, &fx.carrier, "1504fx");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        repo.join(".git").exists(),
        "пустой каталог внутри чужого git-дерева не считается env: stdout={stdout} stderr={stderr}"
    );
    let (branch, _) = git_out(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(
        branch.trim(),
        "work/task-1504fx",
        "создан ворктри с веткой задачи"
    );
    assert!(
        repo.join("carrier-marker.txt").exists(),
        "ворктри несёт код носителя"
    );
    cleanup(fx);
}

#[test]
fn task_env_reuse_refreshes_remote_refs_without_moving_head() {
    let fx = fixture();
    let env_root = fx.root.join("env");
    let first = run_task_env(&env_root, &fx.carrier, "1504fx");
    let stderr_first = String::from_utf8_lossy(&first.stderr);
    assert!(
        first.status.success(),
        "первый вызов создал env: stderr={stderr_first}"
    );
    let repo = env_root.join("1504fx").join("repo");
    let (base_before, _) = git_out(&repo, &["rev-parse", "origin/master"]);

    // Отдельный клон двигает origin: в ворктри env реф остаётся старым.
    let pusher = fx.root.join("pusher");
    let bare = fx.root.join("origin.git");
    git_out(
        &fx.root,
        &[
            "clone",
            "-q",
            bare.to_str().expect("utf8"),
            pusher.to_str().expect("utf8"),
        ],
    );
    std::fs::write(pusher.join("remote-marker.txt"), "origin moved\n").expect("маркер записан");
    git_out(&pusher, &["add", "remote-marker.txt"]);
    git_out(&pusher, &["commit", "-q", "-m", "origin moved"]);
    git_out(&pusher, &["push", "-q", "origin", "master"]);
    git_out(&pusher, &["fetch", "-q", "origin"]);
    let (base_after, _) = git_out(&pusher, &["rev-parse", "origin/master"]);
    assert_ne!(
        base_before.trim(),
        base_after.trim(),
        "arrange: origin сдвинулся"
    );
    let (stale_origin, _) = git_out(&repo, &["rev-parse", "origin/master"]);
    assert_eq!(
        stale_origin.trim(),
        base_before.trim(),
        "arrange: реф env ещё старый"
    );

    let second = run_task_env(&env_root, &fx.carrier, "1504fx");
    let stderr = String::from_utf8_lossy(&second.stderr);

    assert!(
        second.status.success(),
        "повторный вызов зелёный: stderr={stderr}"
    );
    let (env_origin, _) = git_out(&repo, &["rev-parse", "origin/master"]);
    assert_eq!(
        env_origin.trim(),
        base_after.trim(),
        "reuse подтянул рефсы origin"
    );
    let (head, _) = git_out(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(head.trim(), base_before.trim(), "HEAD ворктри не сдвинулся");
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
