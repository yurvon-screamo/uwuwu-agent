//! Процессный e2e write-path docs (docs create/append/replace/delete/tree,
//! задача 46 — стадия тестирования): живой спавн uwuwu-cli против
//! локального git-контура (bare origin + рабочий клон по конвенции
//! from_env; клон готовится заранее, иначе ensure_clone пойдёт в GitHub).
//! Прод-вики тестами не касается. Пины:
//! A. Happy round-trip главного сценария: tree → create (--md) → append →
//!    read (stdout байт-в-байт равен файлу клона — контракт --base) →
//!    replace --base (sha256 от stdout read) → delete → tree.
//! B. replace --base: протухший снапшот — громкий отказ ДО мутации
//!    (файл на origin не перезаписан).
//! C. Легаси-слаг (basename) в write — явный отказ с подсказкой полного
//!    слага (контракт плана 46: basename-фоллбек read в write не
//!    переезжает, расхождение — отказ с подсказкой полного слага).
//!    ПИН БАГА тест-круга: реализация отказывает без подсказки («создай
//!    через docs create») и наводит юзера на дубль дока в корне docs/ —
//!    тест красный до фикса, зелёный после.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const REPO: &str = "local/sandbox";
const CLONE_DIR: &str = "local__sandbox";
const SLUG: &str = "Projects/proj/e2e-doc";
const BASENAME: &str = "e2e-doc";
const SEED_SLUG: &str = "Projects/proj/seed-doc";

fn git(args: &[&str]) {
    let out = Command::new("git").args(args).output().expect("git");
    assert!(
        out.status.success(),
        "git {:?} упал: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// bare origin + рабочий клон с seed-доком; возвращает clone_root для env
/// (from_env: UWUWU_HUB_CLONE_ROOT + <repo-слэши→__>).
fn rig(dir: &Path) -> PathBuf {
    let origin = dir.join("origin");
    let root = dir.join("root");
    let work = root.join(CLONE_DIR);
    std::fs::create_dir_all(&origin).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let (o, w) = (origin.to_str().unwrap(), work.to_str().unwrap());
    git(&["init", "--bare", "-q", "-b", "main", o]);
    git(&["-C", w, "init", "-q", "-b", "main"]);
    git(&["-C", w, "remote", "add", "origin", o]);
    let seed_dir = work.join("docs/Projects/proj");
    std::fs::create_dir_all(&seed_dir).unwrap();
    std::fs::write(
        seed_dir.join("seed-doc.md"),
        "---\ntitle: seed-doc\n---\n\n# seed-doc\n\nтело\n",
    )
    .unwrap();
    git(&["-C", w, "add", "-A"]);
    git(&[
        "-C",
        w,
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        "seed",
    ]);
    git(&["-C", w, "push", "-q", "origin", "main"]);
    root
}

/// Спавн живого uwuwu-cli: env в ребёнка (тест-процесс не грязним).
fn run(clone_root: &Path, stdin: Option<&str>, args: &[&str]) -> (Option<i32>, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_uwuwu-cli"));
    cmd.args(args)
        .env("UWUWU_HUB_REPO", REPO)
        .env("UWUWU_HUB_CLONE_ROOT", clone_root);
    let out = match stdin {
        Some(text) => {
            let mut child = cmd
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn");
            child
                .stdin
                .as_mut()
                .expect("stdin")
                .write_all(text.as_bytes())
                .expect("write stdin");
            child.wait_with_output().expect("wait")
        }
        None => cmd.output().expect("spawn"),
    };
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Контент файла на origin (bare) — факт долетевшей мутации.
fn origin_file(dir: &Path, rel: &str) -> String {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(dir.join("origin"))
        .args(["show", &format!("main:{rel}")])
        .output()
        .expect("git show");
    assert!(out.status.success(), "файла нет на origin: {rel}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// A. Happy round-trip главного сценария (ровно ручной прогон тестировщика).
#[test]
fn docs_write_happy_roundtrip() {
    let dir = tempfile::TempDir::new().unwrap();
    let clone_root = rig(dir.path());

    // tree: seed до мутаций, отсортированный построчно
    let (code, stdout, stderr) = run(&clone_root, None, &["docs", "tree"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(stdout, format!("{SEED_SLUG}\n"));

    // create --md: шапку ведёт create, финальный док в stdout
    let md = dir.path().join("body.md");
    std::fs::write(&md, "тело входа\n").unwrap();
    let (code, stdout, stderr) = run(
        &clone_root,
        None,
        &["docs", "create", SLUG, "--md", md.to_str().unwrap()],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("title: e2e-doc"), "{stdout}");
    assert!(stdout.contains("# e2e-doc"), "{stdout}");
    assert!(stdout.ends_with("тело входа\n"), "{stdout}");

    // append: добавка в хвосте финального дока
    let (code, stdout, stderr) = run(&clone_root, None, &["docs", "append", SLUG, "строка-2"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.ends_with("строка-2\n"), "{stdout}");

    // read: stdout байт-в-байт равен файлу клона — контракт --base
    let (code, stdout, stderr) = run(&clone_root, None, &["read", SLUG]);
    assert_eq!(code, Some(0), "{stderr}");
    let clone_file = std::fs::read_to_string(
        clone_root
            .join(CLONE_DIR)
            .join("docs")
            .join(format!("{SLUG}.md")),
    )
    .unwrap();
    assert_eq!(
        stdout, clone_file,
        "stdout read обязан быть байт-в-байт равен файлу клона (контракт --base)"
    );

    // replace --base (sha256 от stdout read): проходит, дельта на origin
    let newbody = "---\ntitle: e2e-doc\n---\n\n# e2e-doc\n\nперезапись\n";
    let nb = dir.path().join("new.md");
    std::fs::write(&nb, newbody).unwrap();
    let base = sha256_hex(&stdout);
    let (code, stdout, stderr) = run(
        &clone_root,
        None,
        &[
            "docs",
            "replace",
            SLUG,
            "--md",
            nb.to_str().unwrap(),
            "--base",
            &base,
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(stdout, newbody, "replace возвращает финальный док");
    assert_eq!(
        origin_file(dir.path(), &format!("docs/{SLUG}.md")),
        newbody,
        "перезапись доехала до origin"
    );

    // delete отдельного дока: снесён и на origin, tree без него
    let slug2 = "Projects/proj/to-delete";
    let (code, _o, stderr) = run(
        &clone_root,
        Some("удаляемое тело\n"),
        &["docs", "create", slug2, "--stdin"],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let (code, _o, stderr) = run(&clone_root, None, &["docs", "delete", slug2]);
    assert_eq!(code, Some(0), "{stderr}");
    let (code, stdout, stderr) = run(&clone_root, None, &["docs", "tree"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        stdout,
        format!("{SLUG}\n{SEED_SLUG}\n"),
        "tree после delete: сортировка и отсутствие снесённого"
    );
}

/// B. replace с протухшим --base — громкий отказ ДО мутации.
#[test]
fn replace_stale_base_rejects_before_mutation() {
    let dir = tempfile::TempDir::new().unwrap();
    let clone_root = rig(dir.path());
    let md = dir.path().join("body.md");
    std::fs::write(&md, "тело v1\n").unwrap();
    let (code, _o, stderr) = run(
        &clone_root,
        None,
        &["docs", "create", SLUG, "--md", md.to_str().unwrap()],
    );
    assert_eq!(code, Some(0), "{stderr}");

    let stale = "0".repeat(64);
    let (code, _o, stderr) = run(
        &clone_root,
        None,
        &[
            "docs",
            "replace",
            SLUG,
            "--md",
            md.to_str().unwrap(),
            "--base",
            &stale,
        ],
    );
    assert_ne!(code, Some(0), "протухший --base не может пройти как успех");
    assert!(
        stderr.contains("менялся с момента чтения"),
        "отказ объясняет контракт: {stderr}"
    );
    // отказ ДО мутации: на origin шапка от create жива (вход replace
    // «тело v1» без шапки туда не доехал)
    let on_origin = origin_file(dir.path(), &format!("docs/{SLUG}.md"));
    assert!(
        on_origin.contains("title: e2e-doc"),
        "файл не перезаписан при отказе: {on_origin}"
    );
}

/// C. Легаси-слаг (basename) в write: отказ обязан называть полный слаг.
/// ПИН БАГА (см. док-коммент модуля) — красный до фикса.
#[test]
fn write_with_legacy_basename_slug_hints_full_slug() {
    let dir = tempfile::TempDir::new().unwrap();
    let clone_root = rig(dir.path());
    let md = dir.path().join("body.md");
    std::fs::write(&md, "тело\n").unwrap();
    let (code, _o, stderr) = run(
        &clone_root,
        None,
        &["docs", "create", SLUG, "--md", md.to_str().unwrap()],
    );
    assert_eq!(code, Some(0), "{stderr}");

    // док существует по полному пути; basename-слаг в write — отказ,
    // но отказ подсказывает полный слаг (не «создай через create» —
    // та подсказка ведёт юзера к дублю дока в корне docs/)
    let (code, _o, stderr) = run(&clone_root, None, &["docs", "append", BASENAME, "добавка"]);
    assert_ne!(code, Some(0), "легаси-слаг в write не проходит");
    assert!(
        stderr.contains(SLUG),
        "отказ обязан подсказывать полный слаг «{SLUG}»: {stderr}"
    );
}
