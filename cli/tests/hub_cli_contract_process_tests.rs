//! Процессные контракт-тесты CLI на hub-бекене через stub-gh (задача 317,
//! стадия тестирования; носитель клеток переписан задачей 1087 — ADR-098).
//! Живого API нет: `gh` подменяется скриптом на PATH, который обслуживает
//! canned-маршруты REST/GraphQL и ЛОГИРУЕТ argv/stdin вызовов.
//!
//! Почему уровень процесса: контракты, пинуемые здесь, живут на стыке
//! clap-диспатча и store (argv сборка `gh api`/`gh graphql`), юниты
//! функций их не видят.
//!
//! Пины:
//! A. `projects` — gh-контракт «-f/-F без -X = POST»: stub воспроизводит
//!    дефолт gh документированно. Реальный баг 13.11: `projects` POST-ил на
//!    labels (422), печатал тело ошибки как данные и завершался 0.
//! B. `task body --stdin` (append) — тело дописано PATCH'ем как контент;
//!    fm-шапка не появляется (мета в md не живёт — ADR-098).
//! C. `task body --replace --replace-section` — громкий отказ ДО мутации.
//! D. `task body --attach` — вывод честно называет assets/ (не портирован).
//! E. Гвард входа: обёртка `task get` — отказ; fm-шапка во входе — легитимный
//!    контент (карточки больше нет, задача 1087).
//! F. `task body --replace` — тело записано ДОСЛОВНО входом (без fm-переноса
//!    и compose-обвязки).
//! G. `task edit --status` — Pv2-мутация (graphql), PATCH тела НЕТ (fm-письмо
//!    мертво).
//! H. `task edit --branch` — клетка через REST issue-field-values (батч),
//!    без PATCH тела.

use std::path::PathBuf;
use std::process::Command;

const REPO: &str = "owner/hub-guard-test";

/// Canned-issue: чистый контент без fm; клетки — payload issue_field_values.
fn issue_body_md() -> &'static str {
    "# Тело\n\nстрока-1\n"
}

fn issue_json() -> String {
    let body = issue_body_md().replace('\n', "\\n");
    format!(
        r#"{{"number": 999, "title": "Тестовая строка", "state": "open", "body": "{body}", "labels": [{{"name": "proj:uwuwu"}}], "issue_field_values": []}}"#
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
    std::fs::write(
        root.join("err422.json"),
        "{\"message\":\"Invalid request.\\n\\n\\\"name\\\" wasn't supplied.\",\"status\":\"422\"}\n",
    )
    .expect("err422.json");
    // глобальные issue fields (канон владельца): id — фейковые, стабильные
    std::fs::write(
        root.join("fields.json"),
        concat!(
            r#"[{"id":101,"name":"Приоритет"},{"id":102,"name":"Autodone"},{"id":103,"name":"Branch"},"#,
            r#"{"id":104,"name":"PR"},{"id":105,"name":"Сессия dev"},{"id":106,"name":"Расписание"},"#,
            r#"{"id":107,"name":"Запланировано"},{"id":108,"name":"Сессия ревью"},{"id":109,"name":"Сессия тест"}]"#,
        ),
    )
    .expect("fields.json");

    let script = r#"#!/usr/bin/env bash
# stub gh — см. док-коммент модуля. REST: метод = -X если задан; иначе POST
# при наличии -f/-F; иначе GET. GraphQL (--input -): маршрутизация по
# подстрокам stdin-запроса. Canned-ответы — файлами из $STUB_DIR.
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
  case "$a" in
    repos/*) [ -z "$route" ] && route="$a" ;;
    /*) [ -z "$route" ] && route="${a#/}" ;;
  esac
done
DIR="$STUB_DIR"
REPO="owner/hub-guard-test"

# ——— GraphQL: stdin = {query, variables}; маршрутизация по подстрокам ———
if [ "$route" = "" ] && [ "$1" = "api" ] && [ "$2" = "graphql" ]; then
  route="graphql"
fi
if [ "$route" = "graphql" ]; then
  q="$(cat)"
  echo "$q" >> "$STUB_GQLLOG"
  case "$q" in
    *updateProjectV2ItemFieldValue*)
      printf '{"data":{"updateProjectV2ItemFieldValue":{"clientMutationId":"m1"}}}'
      exit 0 ;;
    *addProjectV2ItemById*)
      printf '{"data":{"addProjectV2ItemById":{"item":{"id":"IT_1"}}}}'
      exit 0 ;;
    *fieldValueByName*projectItems*)
      printf '{"data":{"repository":{"issue":{"projectItems":{"nodes":[{"project":{"number":5},"fieldValueByName":{"name":"Todo"}}]}}}}}'
      exit 0 ;;
    *projectItems*)
      printf '{"data":{"repository":{"issue":{"projectItems":{"nodes":[{"id":"IT_1","project":{"id":"PVT_1","number":5}}]}}}}}'
      exit 0 ;;
    *ProjectV2SingleSelectField*)
      printf '{"data":{"node":{"field":{"id":"FLD_ST","options":[{"id":"OPT_D","name":"Draft"},{"id":"OPT_T","name":"Todo"},{"id":"OPT_P","name":"In Progress"}]}}}}'
      exit 0 ;;
    *'projectV2(number: $n) { id }'*)
      printf '{"data":{"viewer":{"projectV2":{"id":"PVT_1"}}}}'
      exit 0 ;;
    *'issue(number: $n) { id }'*)
      printf '{"data":{"repository":{"issue":{"id":"ISS_1"}}}}'
      exit 0 ;;
    *'items(first: 100'*)
      printf '{"data":{"viewer":{"projectV2":{"items":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{"content":{"number":999},"fieldValueByName":{"name":"Todo"}}]}}}}}'
      exit 0 ;;
    *'labels(first: 10)'*)
      printf '{"data":{"repository":{"issue":{"labels":{"nodes":[{"name":"proj:uwuwu"}]}}}}}'
      exit 0 ;;
    *'projectsV2(first: 30)'*)
      printf '{"data":{"viewer":{"projectsV2":{"nodes":[{"number":5,"title":"Задачи uwuwu"}]}}}}'
      exit 0 ;;
  esac
  printf '{"errors":[{"message":"stub: graphql-маршрут не обслужен"}]}'
  exit 1
fi

printf '%s %s\n' "$method" "$route" >> "$STUB_LOG"
if [ "$method" = "POST" ] || [ "$method" = "PATCH" ] || [ "$method" = "DELETE" ]; then
  cat > "$STUB_PAYLOAD"
fi

if [ "$route" = "user/issue-fields" ]; then
  cat "$DIR/fields.json"
  exit 0
fi
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
  "repos/$REPO/issues/999/issue-field-values")
    if [ "$method" = "POST" ]; then printf '[]'; exit 0; fi
    ;;
  "repos/$REPO/issues/999")
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
    let gqllog = root.join("gql-calls.log");
    std::fs::write(&gqllog, "").expect("gql log");
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
        .env("STUB_PAYLOAD", payload)
        .env("STUB_GQLLOG", dir.path().join("gql-calls.log"));
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

fn last_payload_raw(guards: &(tempfile::TempDir, PathBuf, PathBuf, PathBuf)) -> Option<String> {
    let (_, _, payload, _) = guards;
    std::fs::read_to_string(payload).ok()
}

fn gql_log(guards: &(tempfile::TempDir, PathBuf, PathBuf, PathBuf)) -> String {
    let (dir, ..) = guards;
    std::fs::read_to_string(dir.path().join("gql-calls.log")).unwrap_or_default()
}

/// A. `projects`: явный GET на labels + чистый stdout (имена, не JSON-мусор).
#[test]
fn projects_sends_explicit_get_and_prints_clean_names() {
    let guards = setup_stub_gh();
    let (code, stdout, stderr) = run_stub(&guards, None, &["projects"]);
    assert_eq!(code, Some(0), "rc 0; stderr={stderr}");
    assert_eq!(stdout, "uwuwu\noriga\n");
}

/// B. append: тело дописано PATCH'ем как контент, fm-шапка не появляется
/// (ADR-098: мета в md не живёт).
#[test]
fn body_appends_content_without_frontmatter() {
    let guards = setup_stub_gh();
    let (code, _stdout, stderr) = run_stub(
        &guards,
        Some("доб-строка\n"),
        &["task", "body", "999", "--project", "uwuwu", "--stdin"],
    );
    assert_eq!(code, Some(0), "append успешен: {stderr}");
    let written = patch_body(&guards).expect("PATCH записан");
    assert_eq!(
        written, "# Тело\n\nстрока-1\nдоб-строка\n",
        "append = текущее тело + добавка, без fm-обвязки"
    );
    assert!(!written.contains("status:"), "fm-карточка не возникает");
}

/// C. `--replace --replace-section` — не мутит тело: комбинация отсекается
/// (громкий отказ ДО мутации).
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

/// D. `--attach` не теряется молча: вывод называет assets/.
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

/// E. Гвард входа: обёртка `task get` — отказ; fm-шапка — легитимный контент
/// (карточки нет, задача 1087): пишется как есть.
#[test]
fn wrapper_is_rejected_frontmatter_is_content() {
    let guards = setup_stub_gh();
    let (code, _stdout, stderr) = run_stub(
        &guards,
        Some("Задача:   Тестовая строка\nСтатус:   Todo\nСтрока:  issue #999\n\nтело\n"),
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

    let guards2 = setup_stub_gh();
    let (code, _stdout, stderr) = run_stub(
        &guards2,
        Some("---\ntitle: док-шапка как контент\n---\n\nконтент\n"),
        &[
            "task",
            "body",
            "999",
            "--project",
            "uwuwu",
            "--replace",
            "--force",
            "--stdin",
        ],
    );
    assert_eq!(
        code,
        Some(0),
        "fm во входе = контент, пишется как есть: {stderr}"
    );
    let written = patch_body(&guards2).expect("PATCH записан");
    assert_eq!(
        written,
        "---\ntitle: док-шапка как контент\n---\n\nконтент\n"
    );
}

/// F. `--replace` — тело записано ДОСЛОВНО входом (compose-обвязка снесена).
#[test]
fn replace_writes_body_verbatim() {
    let guards = setup_stub_gh();
    let md = guards.0.path().join("content.md");
    std::fs::write(&md, "полностью новое тело\n").expect("content.md");
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
            "--md",
            md.to_str().unwrap(),
        ],
    );
    assert_eq!(code, Some(0), "replace успешен: {stderr}");
    let written = patch_body(&guards).expect("PATCH записан");
    assert_eq!(written, "полностью новое тело\n", "тело без обвязки");
}

/// G. `task edit --status` — Pv2-мутация (graphql updateProjectV2ItemFieldValue),
/// PATCH тела НЕТ (fm-письмо мертво).
#[test]
fn edit_status_goes_to_project_v2_not_body() {
    let guards = setup_stub_gh();
    let (code, _stdout, stderr) = run_stub(
        &guards,
        None,
        &[
            "task",
            "edit",
            "999",
            "--project",
            "uwuwu",
            "--status",
            "In Progress",
        ],
    );
    assert_eq!(
        code,
        Some(0),
        "edit успешен: {stderr} gql: {}",
        gql_log(&guards)
    );
    let gql = gql_log(&guards);
    assert!(
        gql.contains("updateProjectV2ItemFieldValue"),
        "статус через Pv2-мутацию: {gql}"
    );
    let rest = std::fs::read_to_string(guards.1.clone()).unwrap_or_default();
    assert!(
        !rest.contains("PATCH repos/owner/hub-guard-test/issues/999"),
        "PATCH тела (fm-письмо) отсутствует: {rest}"
    );
    assert!(patch_body(&guards).is_none(), "тело не мутировано");
}

/// H. `task edit --branch` — клетка через REST issue-field-values (батч),
/// без PATCH тела; значение — text-строка.
#[test]
fn edit_branch_goes_to_issue_field_values() {
    let guards = setup_stub_gh();
    let (code, _stdout, stderr) = run_stub(
        &guards,
        None,
        &[
            "task",
            "edit",
            "999",
            "--project",
            "uwuwu",
            "--branch",
            "work/task-9",
        ],
    );
    assert_eq!(code, Some(0), "edit успешен: {stderr}");
    let payload = last_payload_raw(&guards).expect("POST issue-field-values записан");
    let v: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
    let cells = v["issue_field_values"].as_array().expect("cells");
    assert_eq!(cells.len(), 1, "одна клетка: {payload}");
    assert_eq!(cells[0]["field_id"].as_i64(), Some(103), "Branch field id");
    assert_eq!(cells[0]["value"].as_str(), Some("work/task-9"));
    let rest = std::fs::read_to_string(guards.1.clone()).unwrap_or_default();
    assert!(
        !rest.contains("PATCH repos/owner/hub-guard-test/issues/999"),
        "PATCH тела отсутствует: {rest}"
    );
}
