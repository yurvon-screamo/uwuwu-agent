//! E2E клон-менеджмента на уровне процесса (задача 1062, стадия тестирования):
//! живая команда `read` против изолированного file://-рига через
//! UWUWU_HUB_CLONE_ROOT — тот сценарий, что прогоняется руками, только
//! без сети (origin — локальный bare).
//!
//! Почему уровень процесса: контракты задачи живут в связке «git-риг формы
//! cli + tracing-warn в stderr живой команды» — юниты store (модульные
//! github_doc_clone_sync_tests) stderr живого процесса не видят.
//!
//! Пины:
//! A. Главный путь (behind-синк): origin ушёл вперёд, дерево чистое —
//!    живая `read` синкает клон и печатает свежий контент (rc 0, stderr
//!    без warn).
//! B. Грязный клон: живая `read` — rc 0 + warn «грязный» в stderr + чужая
//!    незакоммиченная правка жива (stale-read вместо громкого отказа).
//! C. Незапушенный коммит: живая `read` — rc 0 + warn «незапушенный» в
//!    stderr + коммит пережил sync (инцидент сноса незапушенных коммитов).

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

/// Риг формы cli: bare origin + shallow-клон в каталоге, куда cli смотрит
/// через UWUWU_HUB_CLONE_ROOT. Возвращает (tmp, origin, clone_dir).
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

    // сид-контент через writer-клон
    let writer = root.join(format!("{name}-writer"));
    git(&["init", "-q", "-b", "main", writer.to_str().unwrap()]);
    let person = writer.join("docs/Experience/person/PERSON.md");
    std::fs::create_dir_all(person.parent().unwrap()).expect("docs dir");
    std::fs::write(&person, "база\n").expect("seed person");
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
        "X1",
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

    (tmp, origin, clone)
}

/// Пуш v2 из writer: маркерная строка в PERSON.md.
fn writer_push(origin: &Path, writer: &Path, tag: &str) {
    let person = writer.join("docs/Experience/person/PERSON.md");
    let mut body = std::fs::read_to_string(&person).expect("read person");
    body.push_str(tag);
    body.push('\n');
    std::fs::write(&person, body).expect("write person");
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
        tag,
    ]);
    git(&[
        "-C",
        writer.to_str().unwrap(),
        "push",
        "-q",
        origin.to_str().unwrap(),
        "main",
    ]);
}

/// Живой вызов cli: `read` с env, изолирующим клон-каталог.
fn cli_read(clone_root: &Path) -> (Option<i32>, String, String) {
    let out = Command::new(bin())
        .arg("read")
        .arg("Experience/person/PERSON")
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

/// A. Главный путь: behind-синк живой командой — клон догоняет origin,
/// контент свежий, stderr чист (нет warn на здоровом пути).
#[test]
fn read_command_syncs_behind_clone_and_prints_new_content() {
    let (tmp, origin, clone) = setup_rig("e2e-behind");
    let writer = tmp.path().join("e2e-behind-writer");
    writer_push(&origin, &writer, "X2");

    let (code, stdout, stderr) = cli_read(tmp.path().join("hub-clone").as_path());
    assert_eq!(code, Some(0), "read упал: {stderr}");
    assert!(
        stdout.contains("X2"),
        "behind-синк не подтянул свежий контент: {stdout}"
    );
    assert!(
        !stderr.contains("WARN"),
        "здоровый синк не должен предупреждать: {stderr}"
    );

    let head = Command::new("git")
        .args(["-C", clone.to_str().unwrap(), "rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse");
    let bare = Command::new("git")
        .args(["-C", origin.to_str().unwrap(), "rev-parse", "main"])
        .output()
        .expect("git rev-parse");
    assert_eq!(
        String::from_utf8_lossy(&head.stdout),
        String::from_utf8_lossy(&bare.stdout),
        "клон не догнал origin за живой командой"
    );
}

/// B. Грязный клон: живая read — Ok + warn + чужая правка жива.
#[test]
fn read_command_on_dirty_clone_warns_and_preserves_files() {
    let (_tmp, _origin, clone) = setup_rig("e2e-dirty");
    let draft = clone.join("docs/draft.md");
    std::fs::write(&draft, "чужой черновик\n").expect("dirty file");

    let clone_root = clone.parent().unwrap().to_path_buf();
    let (code, stdout, stderr) = cli_read(&clone_root);
    assert_eq!(code, Some(0), "dirty-read должен быть Ok+stale: {stderr}");
    assert_eq!(stdout, "база\n", "dirty-read отдал не stale-контент");
    assert!(
        stderr.contains("грязный"),
        "warn о грязи не дошёл до stderr живой команды: {stderr}"
    );
    assert!(
        draft.exists(),
        "чужая незакоммиченная правка снесена живой командой"
    );
}

/// C. Незапушенный коммит переживает живую read-sync: warn + коммит цел.
#[test]
fn read_command_on_ahead_clone_warns_and_preserves_commit() {
    let (_tmp, _origin, clone) = setup_rig("e2e-ahead");
    let ahead = clone.join("docs/Experience/ahead/A.md");
    std::fs::create_dir_all(ahead.parent().unwrap()).expect("dir");
    std::fs::write(&ahead, "локальная правка\n").expect("ahead file");
    git(&["-C", clone.to_str().unwrap(), "add", "-A"]);
    git(&[
        "-C",
        clone.to_str().unwrap(),
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-q",
        "-m",
        "X1b",
    ]);

    let clone_root = clone.parent().unwrap().to_path_buf();
    let (code, _stdout, stderr) = cli_read(&clone_root);
    assert_eq!(code, Some(0), "ahead-read должен быть Ok+warn: {stderr}");
    assert!(
        stderr.contains("незапушенный"),
        "warn о незапушенном коммите не дошёл до stderr: {stderr}"
    );
    assert!(
        ahead.exists(),
        "незапушенный коммит снесён живой read-командой"
    );
}
