//! Процессные контракт-тесты CLI на hub-бекене через stub-gh (задача 317,
//! стадия тестирования). Живого API нет: `gh` подменяется скриптом на PATH,
//! который обслуживает canned-маршруты REST и ЛОГИРУЕТ argv/stdin вызовов.
//!
//! Почему уровень процесса: контракты, пинуемые здесь, живут на стыке
//! clap-диспатча и store (argv сборка `gh api`), юниты функций их не видят.
//!
//! Пины:
//! A. `projects` — gh-контракт «-f/-F без -X = POST»: stub воспроизводит
//!    дефолт gh документированно (gh api: при наличии полей метод POST,
//!    если -X не задан). Реальный баг 13.11: `projects` POST-ил на labels
//!    (422 «name wasn't supplied»), печатал тело ошибки как данные и
//!    завершался 0. Пин: явный GET в argv + чистый список имён + rc 0.
//! B. `task body --stdin` (append) — fm-карточка тела переживает запись
//!    без задвоения (постановка: «тело — чистовик, карточку ведёт store»).
//! C. `task body --replace --replace-section` — громкий отказ (help
//!    документирует секционную хирургию; тихая подмена semantics —
//!    молчаливая порча тела). Контракт честности: отказ ДО мутации.
//! D. `task body --attach` — вход не теряется молча: вывод обязан честно
//!    сказать про assets/ (подсказка или загрузка), не «успех» с потерей.
//! E. Гвард входа тела: frontmatter-шапка и обёртка `task get` во входе —
//!    отказ (контракт help: «Вход — контент тела, не обёртка task get»).

use std::path::PathBuf;
use std::process::Command;

const REPO: &str = "owner/hub-guard-test";

/// Canned-issue: fm-карточка + контент. convert() читает number/title/body/
/// state и fm-поля (status/branch/...).
fn issue_body_md() -> &'static str {
    "---\nstatus: Todo\nbranch: work/x\npr: \npriority: medium\n---\n\n# Тело\n\nстрока-1\n"
}

fn issue_json() -> String {
    // body в json-строке: экранируем переносы руками — canned, не общий парсер
    let body = issue_body_md().replace('\n', "\\n");
    format!(
        r#"{{"number": 999, "title": "Тестовая строка", "state": "open", "body": "{body}", "labels": [{{"name": "proj:uwuwu"}}]}}"#
    )
}

/// Canned-ответы живут ФАЙЛАМИ (байтово точно из Rust), bash только
/// маршрутизирует и делает cat — никакой интерполяции JSON в скрипт.
fn setup_stub_gh() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let dir = tempfile::TempDir::new().expect("tmpdir");
    let root = dir.path();
    let bin_dir = root.join("bin");
    let log = root.join("calls.log");
    let payload = root.join("last-patch.json");
    std::fs::create_dir(&bin_dir).expect("bin dir");

    let issue = issue_json();
    std::fs::write(root.join("issue.json"), format!("{issue}\n")).expect("issue.json");
    std::fs::write(root.join("list.json"), format!("[{issue}]\n")).expect("list.json");
    // тело ошибки GitHub на POST labels: печатается в stdout при exit 1
    // (gh печатает тело ответа и падает; cli stdout-путь получает его как данные)
    std::fs::write(
        root.join("err422.json"),
        "{\"message\":\"Invalid request.\\n\\n\\\"name\\\" wasn't supplied.\",\"documentation_url\":\"https://docs.github.com/rest/issues/labels#create-a-label\",\"status\":\"422\"}\n",
    )
    .expect("err422.json");

    let script = r#"#!/usr/bin/env bash
# stub gh — см. док-коммент модуля. Эмулирует gh-контракт: метод = -X если
# задан; иначе POST при наличии -f/-F; иначе GET (документированное
# поведение gh api). Canned-ответы — файлами из $STUB_DIR.
set -u
method=""
prev=""
for a in "$@"; do
  if [ "$prev" = "-X" ]; then method="$a"; fi
  case "$a" in -f|-F) [ -z "$method" ] && method=POST ;; esac
  prev="$a"
done
[ -z "$method" ] && method=GET
route=""
for a in "$@"; do
  case "$a" in repos/*) [ -z "$route" ] && route="$a" ;; esac
done
printf '%s %s\n' "$method" "$route" >> "$STUB_LOG"
DIR="$STUB_DIR"
REPO="owner/hub-guard-test"
if [ "$route" = "repos/$REPO/labels" ]; then
  if [ "$method" = "POST" ]; then
    cat "$DIR/err422.json"
    exit 1
  fi
  printf 'uwuwu\noriga\n'
  exit 0
fi
case "$route" in
  "repos/$REPO/issues?"*)
    cat "$DIR/list.json"
    exit 0
    ;;
  "repos/$REPO/issues/999")
    if [ "$method" = "PATCH" ]; then
      cat > "$STUB_PAYLOAD"
    fi
    cat "$DIR/issue.json"
    exit 0
    ;;
esac
printf '{"message":"stub: маршрут не обслужен"}\n'
exit 1
"#;
    let gh = bin_dir.join("gh");
    std::fs::write(&gh, script).expect("stub script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    (dir, log, payload, gh)
}

fn run_stub(
    guards: &(tempfile::TempDir, PathBuf, PathBuf, PathBuf),
    stdin: Option<&str>,
    args: &[&str],
) -> (Option<i32>, String, String) {
    let (dir, log, payload, gh) = guards;
    std::fs::write(log, "").expect("reset log");
    let _ = std::fs::remove_file(payload);
    let real_path = std::env::var("PATH").unwrap_or_default();
    let stub_path = format!("{}:{real_path}", gh.parent().unwrap().display());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_uwuwu-cli"));
    cmd.args(args)
        .env("PATH", stub_path)
        .env("UWUWU_HUB_REPO", REPO)
        .env("STUB_DIR", dir.path())
        .env("STUB_LOG", log)
        .env("STUB_PAYLOAD", payload);
    match stdin {
        Some(text) => {
            use std::io::Write;
            let mut child = cmd
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn");
            child
                .stdin
                .as_mut()
                .expect("stdin")
                .write_all(text.as_bytes())
                .expect("write stdin");
            let out = child.wait_with_output().expect("wait");
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        }
        None => {
            let out = cmd.output().expect("spawn");
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        }
    }
}

fn patch_body(guards: &(tempfile::TempDir, PathBuf, PathBuf, PathBuf)) -> Option<String> {
    let (_, _, payload, _) = guards;
    let raw = std::fs::read_to_string(payload).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).expect("patch payload json");
    Some(v["body"].as_str().expect("body str").to_string())
}

/// A. `projects`: явный GET на labels + чистый stdout (имена, не JSON-мусор).
#[test]
fn projects_sends_explicit_get_and_prints_clean_names() {
    let guards = setup_stub_gh();
    let (code, stdout, stderr) = run_stub(&guards, None, &["projects"]);
    assert_eq!(code, Some(0), "rc 0; stderr={stderr}");
    // контракт фикса: имена проектов, ничего сверх (до фикса — тело
    // ошибки 422 как данные, rc 0)
    assert_eq!(stdout, "uwuwu\noriga\n");
}

/// B. append переживает fm-карточку: блок сохранён, не задвоен, addition в конце.
#[test]
fn body_append_preserves_frontmatter_card() {
    let guards = setup_stub_gh();
    let (code, _stdout, stderr) = run_stub(
        &guards,
        Some("доб-строка\n"),
        &["task", "body", "999", "--project", "uwuwu", "--stdin"],
    );
    assert_eq!(code, Some(0), "append успешен: {stderr}");
    let written = patch_body(&guards).expect("PATCH записан");
    assert!(
        written.starts_with("---\n"),
        "fm-карточка в начале: {written}"
    );
    assert!(written.contains("status: Todo"), "fm-поле живо: {written}");
    assert!(written.contains("# Тело"), "контент жив: {written}");
    assert!(
        written.ends_with("доб-строка\n"),
        "addition в конце: {written}"
    );
    assert_eq!(
        written.matches("\n---\n").count(),
        1,
        "fm не задвоен: {written}"
    );
}

/// C. `--replace --replace-section` — не мутит тело: комбинация отсекается
/// (clap-конфликт флагов — usage-error с именами флагов). Пин честности:
/// секционная правка с --replace не может молча перезаписать тело
/// ЦЕЛИКОМ секционным контентом.
#[test]
fn replace_with_section_flag_cannot_silently_overwrite_body() {
    let guards = setup_stub_gh();
    let md = guards.0.path().join("sec.md");
    std::fs::write(&md, "новое содержимое секции\n").expect("sec.md");
    let (code, _stdout, stderr) = run_stub(
        &guards,
        None,
        &[
            "task",
            "body",
            "999",
            "--project",
            "uwuwu",
            "--replace",
            "--force",
            "--replace-section",
            "Решение",
            "--md",
            md.to_str().unwrap(),
        ],
    );
    assert_ne!(code, Some(0), "комбинация не может пройти как успех");
    assert!(
        stderr.contains("--replace-section"),
        "отказ называет флаг: {stderr}"
    );
    assert!(
        patch_body(&guards).is_none(),
        "отказ ДО мутации: тело не перезаписано"
    );
}

/// D. `--attach` не теряется молча: вывод называет assets/ (подсказка или
/// успех загрузки), не «успех» с потерей входа.
#[test]
fn attach_call_names_assets_in_output() {
    let guards = setup_stub_gh();
    let md = guards.0.path().join("content.md");
    std::fs::write(&md, "новое тело\n").expect("content.md");
    let file = guards.0.path().join("shot.png");
    std::fs::write(&file, b"png-bytes").expect("attach file");
    let (code, stdout, stderr) = run_stub(
        &guards,
        None,
        &[
            "task",
            "body",
            "999",
            "--project",
            "uwuwu",
            "--replace",
            "--force",
            "--md",
            md.to_str().unwrap(),
            "--attach",
            file.to_str().unwrap(),
        ],
    );
    assert_eq!(code, Some(0), "replace успешен: {stderr}");
    let said = format!("{stdout}{stderr}");
    assert!(
        said.contains("assets/"),
        "вывод обязан честно сказать про assets/ (подсказка или загрузка): {said}"
    );
}

/// E. Гвард входа: frontmatter-шапка и обёртка `task get` — отказ (контракт
/// help «Вход — контент тела»; шапка в теле = мусор карточки).
#[test]
fn body_input_rejects_frontmatter_and_task_get_wrapper() {
    let guards = setup_stub_gh();
    let (code, _stdout, stderr) = run_stub(
        &guards,
        Some("---\ntitle: чужой fm\n---\n\nконтент\n"),
        &["task", "body", "999", "--project", "uwuwu", "--stdin"],
    );
    assert_eq!(code, Some(1), "fm-шапка во входе — отказ: {stderr}");
    assert!(
        stderr.contains("frontmatter") || stderr.contains("контент тела"),
        "отказ объясняет контракт: {stderr}"
    );

    let (code, _stdout, stderr) = run_stub(
        &guards,
        Some("Задача:   Тестовая строка\nСтатус:   Todo\nСтрока:  issue #999\n\n---\n\nтело\n"),
        &["task", "body", "999", "--project", "uwuwu", "--stdin"],
    );
    assert_eq!(code, Some(1), "обёртка task get во входе — отказ: {stderr}");
    assert!(
        stderr.contains("обёртка") || stderr.contains("контент тела"),
        "отказ объясняет контракт: {stderr}"
    );
    assert!(
        patch_body(&guards).is_none(),
        "отказ ДО мутации: тело не тронуто"
    );
}
