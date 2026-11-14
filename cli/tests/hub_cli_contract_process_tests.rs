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
//! I. `cron list`: полный обход страниц (мастер за краем первой сотни не
//!    теряется молча), labels-фильтр в query-string (канон GET-выборки,
//!    e2e 13.11), PR-строки пропущены (REST /issues возвращает и PR);
//!    расписание — глобальное поле (ADR-098), не fm.
//! J. `projects`: labels обходят страницы — проект за краем первой сотни
//!    не теряется (sibling того же класса, ADR-020).

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

/// Canned-issue с клеткой «Расписание» (ADR-098) — крон-мастер для пина I.
fn cron_master(number: u64, id: u64, title: &str) -> String {
    let schedule_cell =
        r#""issue_field_values": [{"issue_field_name": "Расписание", "value": "5m"}]"#;
    format!(
        r#"{{"id": {id}, "number": {number}, "title": "{title}", "state": "open", "body": "тело крона", "labels": [{{"name": "proj:uwuwu"}}], {schedule_cell}}}"#
    )
}

/// Canned-issue без расписания — наполнитель полной страницы.
fn plain_issue(number: u64, id: u64) -> String {
    format!(
        r#"{{"id": {id}, "number": {number}, "title": "обычная-{number}", "state": "open", "body": "без расписания", "labels": [{{"name": "proj:uwuwu"}}], "issue_field_values": []}}"#
    )
}

/// Canned-PR с расписанием в клетке: REST /issues возвращает и PR —
/// cron list обязан его пропустить (пин I).
fn pr_with_schedule(number: u64, id: u64) -> String {
    format!(
        r#"{{"id": {id}, "number": {number}, "title": "pr-с-расписанием", "state": "open", "body": "тело pr", "pull_request": {{"url": "https://example/pr/{number}"}}, "labels": [{{"name": "proj:uwuwu"}}], "issue_field_values": [{{"issue_field_name": "Расписание", "value": "5m"}}]}}"#
    )
}

fn issues_page_json(items: &[String]) -> String {
    format!("[{}]\n", items.join(","))
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
    // страницы issues (пины I): дефолт = list.json; пин I перезаписывает
    std::fs::write(root.join("issues-page1.json"), format!("[{issue}]\n"))
        .expect("issues-page1.json");
    std::fs::write(root.join("issues-page2.json"), "[]\n").expect("issues-page2.json");
    // labels canned JSON-ом: gh_labels парсит JSON в Rust (gh_pages);
    // страницы маршрутизируются по page= (пин J)
    std::fs::write(
        root.join("labels-page1.json"),
        "[{\"name\": \"proj:uwuwu\"}, {\"name\": \"proj:origa\"}, {\"name\": \"misc\"}]\n",
    )
    .expect("labels-page1.json");
    std::fs::write(root.join("labels-page2.json"), "[]\n").expect("labels-page2.json");
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
      printf '{"data":{"repository":{"issue":{"projectItems":{"nodes":[{"project":{"number":5},"fieldValueByName":{"name":"In Progress"}}]}}}}}'
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
      printf '{"data":{"viewer":{"projectV2":{"items":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{"content":{"number":999},"fieldValueByName":{"name":"In Progress"}}]}}}}}'
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
if [[ "$route" == "repos/$REPO/labels"* ]]; then
  if [ "$method" = "POST" ]; then
    cat "$DIR/err422.json"
    exit 1
  fi
  case "$route" in
    *page=2*) cat "$DIR/labels-page2.json" ;;
    *) cat "$DIR/labels-page1.json" ;;
  esac
  exit 0
fi
case "$route" in
  "repos/$REPO/issues/999/issue-field-values")
    if [ "$method" = "POST" ]; then printf '[]'; exit 0; fi
    ;;
  "repos/$REPO/issues/999/issue-field-values/"*)
    # очистка несуществующего значения: 404, тело в stdout + stderr (реальный gh)
    printf '{"message":"Issue field value not found","status":"404"}'
    echo "gh: Issue field value not found (HTTP 404)" >&2
    exit 1
    ;;
  "repos/$REPO/issues/999")
    cat "$DIR/issue.json"
    exit 0
    ;;
  "repos/$REPO/issues/4040")
    # Пин 404-детекта direct GET (склейка 1097): 404 — «честный промах»,
    # транспортные коды (403 rate limit) — громкий отказ. Маркер-файл
    # direct-403 переключает режим; stderr gh несёт статус-маркер.
    if [ -f "$STUB_DIR/direct-403" ]; then
      printf 'gh: API rate limit exceeded (HTTP 403)' >&2
    else
      printf 'gh: Not Found (HTTP 404)' >&2
    fi
    exit 1
    ;;
  "repos/$REPO/issues"?*)
    case "$route" in
      *page=2*) cat "$DIR/issues-page2.json" ;;
      *) cat "$DIR/issues-page1.json" ;;
    esac
    exit 0
    ;;
  "repos/$REPO/issues")
    cat "$DIR/issues-page1.json"
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
/// гардoм входа (громкий отказ ДО мутации; комбинации, нарушающие
/// requires base/force, отсекает clap usage-error — осознанно без рецепта).
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

/// L. `--replace-section` без `--replace` со входом — не мутит тело:
/// флаг громко отказывает с рецептом ДО сети и мутации (было: тихий
/// игнор — append-ветка дописывала вход в конец тела и рапортовала
/// успех; при существующей секции — тот самый h2-дубль, задача 67).
#[test]
fn replace_section_without_replace_refuses_loudly_no_mutation() {
    let guards = setup_stub_gh();
    let md = guards.0.path().join("sec.md");
    std::fs::write(&md, "## Решение\nновое содержимое секции\n").expect("sec.md");
    let (code, _stdout, stderr) = run_stub(
        &guards,
        None,
        &[
            "task",
            "body",
            "999",
            "--project",
            "uwuwu",
            "--replace-section",
            "Решение",
            "--md",
            md.to_str().unwrap(),
        ],
    );
    assert_ne!(code, Some(0), "флаг не может пройти как успех");
    assert!(
        stderr.contains("--replace-section"),
        "отказ называет флаг: {stderr}"
    );
    assert!(
        patch_body(&guards).is_none(),
        "отказ ДО мутации: тело не дописано"
    );
}

/// M. `--replace-section` без stdin/--md — громкий отказ, тело не
/// печатается как успех (было: read-ветка молча игнорила флаг и
/// печатала тело; задача 67).
#[test]
fn replace_section_without_input_refuses_loudly() {
    let guards = setup_stub_gh();
    let (code, _stdout, stderr) = run_stub(
        &guards,
        None,
        &[
            "task",
            "body",
            "999",
            "--project",
            "uwuwu",
            "--replace-section",
            "Решение",
        ],
    );
    assert_ne!(code, Some(0), "флаг не может пройти как успех");
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

/// K. Гвард --base сверяет ВЕРБАТИМ: снапшот с хвостовым \n (как даёт
/// `gh api --jq .body` — jq всегда добавляет завершающий перенос) не
/// матчится и replace отказывает, тело не затирается. Пин живой пробы
/// тест-раунда 1087 (14.11): help long_about «task body» обещает матч для
/// jq-снапшота — рецепт неисполним (лишний \n), чинится правкой док-текста
/// (cli.rs, long_about), не гварда: параллельная правка по-прежнему громкая.
#[test]
fn replace_base_rejects_snapshot_with_trailing_jq_newline() {
    let guards = setup_stub_gh();
    let md = guards.0.path().join("content.md");
    std::fs::write(&md, "новое тело\n").expect("content.md");
    // jq-снапшот: canned-тело + хвостовой перенос от jq
    let snap = guards.0.path().join("snapshot-jq.md");
    std::fs::write(&snap, format!("{}\n", issue_body_md())).expect("snapshot");
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
            "--md",
            md.to_str().unwrap(),
            "--base",
            snap.to_str().unwrap(),
        ],
    );
    assert_ne!(code, Some(0), "jq-хвост ≠ тело — гвард обязан отказать");
    assert!(
        stderr.contains("тело изменилось"),
        "внятное сообщение гварда: {stderr}"
    );
    assert!(
        patch_body(&guards).is_none(),
        "отказ до мутации: PATCH тела нет"
    );
}

/// L. Задокументированный рецепт снапшота (long_about `task body`):
/// `gh api … | jq -j '.body' > snap.md` — сырые байты тела без хвостового
/// переноса → гвард проходит, replace пишет новое тело. Комплемент пина K:
/// фикс help-а тест-раунда 1 (5215555) — рецепт теперь честен, оба теста
/// кодируют пару «jq -j проходит / gh --jq отвалится на байте».
#[test]
fn replace_base_passes_snapshot_from_documented_recipe() {
    let guards = setup_stub_gh();
    let md = guards.0.path().join("content.md");
    std::fs::write(&md, "новое тело по рецепту\n").expect("content.md");
    // jq -j: сырые байты body, хвостового переноса нет
    let snap = guards.0.path().join("snapshot-jq-j.md");
    std::fs::write(&snap, issue_body_md()).expect("snapshot");
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
            "--md",
            md.to_str().unwrap(),
            "--base",
            snap.to_str().unwrap(),
        ],
    );
    assert_eq!(code, Some(0), "рецепт help-а проходит гвард: {stderr}");
    let written = patch_body(&guards).expect("PATCH записан");
    assert_eq!(written, "новое тело по рецепту\n", "тело дословно");
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

/// I. task get — Status списка читается из Pv2-джойна доски (canned
/// «In Progress» ≠ Todo-дефолт — фоллбэк не спутать с каноном; точечный
/// read-after-write матч High-1 пинится юнитом item_status).
#[test]
fn task_get_status_comes_from_project_v2_join() {
    let guards = setup_stub_gh();
    let (code, stdout, stderr) =
        run_stub(&guards, None, &["task", "get", "999", "--project", "uwuwu"]);
    assert_eq!(code, Some(0), "get успешен: {stderr}");
    assert!(
        stdout.contains("In Progress"),
        "статус из Pv2-джойна (canned In Progress, не Todo-фоллбэк): {stdout}"
    );
}

/// J. Очистка клетки: `--branch ""` → DELETE → 404 на отсутствующем значении
/// → no-op (rc 0): идемпотентность повторных клинапов (ревью round-2 High).
#[test]
fn edit_clear_branch_tolerates_404_on_missing_value() {
    let guards = setup_stub_gh();
    let (code, _stdout, stderr) = run_stub(
        &guards,
        None,
        &["task", "edit", "999", "--project", "uwuwu", "--branch", ""],
    );
    assert_eq!(code, Some(0), "очистка пустой клетки — no-op: {stderr}");
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

/// I. `cron list`: полный обход страниц — мастер со 2-й страницы не теряется
/// (класс бага 1086: молчащее усечение первой сотней); labels-фильтр в
/// query-string (канон GET-выборки, e2e 13.11); PR-строки пропущены;
/// расписание читается из клетки ADR-098 (не fm).
#[test]
fn cron_list_walks_pages_filters_via_querystring_and_skips_prs() {
    let guards = setup_stub_gh();
    let (dir, log, _payload, _) = &guards;
    let dir = dir.path();
    // Arrange: страница 1 полная (100) — 2 мастера, PR с расписанием,
    // 97 обычных; страница 2 короткая — мастер за краем первой сотни
    let mut page1: Vec<String> = Vec::new();
    page1.push(cron_master(11, 1011, "cron-master-один"));
    page1.push(cron_master(12, 1012, "cron-master-два"));
    page1.push(pr_with_schedule(300, 1300));
    for i in 0..97u64 {
        page1.push(plain_issue(20 + i, 2000 + i));
    }
    std::fs::write(dir.join("issues-page1.json"), issues_page_json(&page1)).expect("page1");
    std::fs::write(
        dir.join("issues-page2.json"),
        issues_page_json(&[cron_master(400, 1400, "cron-master-страница-два")]),
    )
    .expect("page2");

    // Act
    let (code, stdout, stderr) = run_stub(&guards, None, &["cron", "list", "--project", "uwuwu"]);

    // Assert: инвентарь полон, формат вывода не изменился
    assert_eq!(code, Some(0), "rc 0; stderr={stderr}");
    assert!(
        stdout.contains("cron-master-один"),
        "мастер стр.1: {stdout}"
    );
    assert!(stdout.contains("cron-master-два"), "мастер стр.1: {stdout}");
    assert!(
        stdout.contains("cron-master-страница-два"),
        "мастер со 2-й страницы не потерян: {stdout}"
    );
    assert!(
        !stdout.contains("pr-с-расписанием"),
        "PR пропущен: {stdout}"
    );
    assert!(
        stdout.contains("крон-мастеров: 3"),
        "итог честный: {stdout}"
    );

    // argv-контракт: две страницы, query-string с urlencoded labels, GET
    let calls = std::fs::read_to_string(log).expect("log");
    let issue_calls: Vec<&str> = calls.lines().filter(|l| l.contains("/issues?")).collect();
    assert_eq!(issue_calls.len(), 2, "обе страницы запрошены: {calls}");
    assert!(
        issue_calls.iter().all(|l| l.starts_with("GET ")),
        "query-string форма — GET без -F: {calls}"
    );
    assert!(
        issue_calls
            .iter()
            .all(|l| l.contains("labels=proj%3Auwuwu")),
        "labels в query-string, colon urlencoded: {calls}"
    );
    assert!(
        issue_calls.iter().any(|l| l.contains("page=1"))
            && issue_calls.iter().any(|l| l.contains("page=2")),
        "номера страниц в argv: {calls}"
    );
}

/// J. `projects`: labels обходят страницы — проект за краем первой сотни
/// не теряется (sibling bug-класса 1086, канон ADR-020).
#[test]
fn projects_walks_label_pages() {
    let guards = setup_stub_gh();
    let (dir, log, _payload, _) = &guards;
    let dir = dir.path();
    // Arrange: страница 1 полная (100), proj:uwuwu на ней; proj:origa —
    // на короткой странице 2
    let mut page1: Vec<String> = vec![r#"{"name": "proj:uwuwu"}"#.into()];
    for i in 0..99u64 {
        page1.push(format!(r#"{{"name": "наполнитель-{i}"}}"#));
    }
    std::fs::write(
        dir.join("labels-page1.json"),
        format!("[{}]\n", page1.join(",")),
    )
    .expect("labels page1");
    std::fs::write(
        dir.join("labels-page2.json"),
        "[{\"name\": \"proj:origa\"}]\n",
    )
    .expect("labels page2");

    // Act
    let (code, stdout, stderr) = run_stub(&guards, None, &["projects"]);

    // Assert
    assert_eq!(code, Some(0), "rc 0; stderr={stderr}");
    assert_eq!(
        stdout, "uwuwu\noriga\n",
        "проект со 2-й страницы не потерян: {stdout}"
    );
    let calls = std::fs::read_to_string(log).expect("log");
    let label_calls: Vec<&str> = calls.lines().filter(|l| l.contains("/labels")).collect();
    assert_eq!(
        label_calls.len(),
        2,
        "обе страницы labels запрошены: {calls}"
    );
}

/// F. Канон резолва, direct GET (граница gh-процесса, склейка 1097):
/// numeric-needle после miss по open-списку идёт точечным GET issues/<N>.
/// 404 — «честный промах» → substring-фоллбек → «не найдено» (не
/// транспортная авария). Пин уровня процесса: юнит is_github_not_found
/// не видит ни argv-сборку direct GET, ни склейку stderr в цепочку ошибки.
#[test]
fn direct_get_404_after_list_miss_falls_back_to_not_found() {
    let guards = setup_stub_gh();
    let (code, stdout, stderr) = run_stub(
        &guards,
        None,
        &["task", "get", "4040", "--project", "uwuwu", "--no-body"],
    );
    let said = format!("{stdout}{stderr}");
    assert!(
        said.contains("не найдено"),
        "404 direct GET → substring-фоллбек → «не найдено»: {said}"
    );
    assert!(
        !said.contains("HTTP 404") && !said.contains("упал"),
        "404 не всплывает как транспортная авария: {said}"
    );
    assert_eq!(code, Some(1), "отказ rc 1");
}

/// G. Регресс Common-ревью 1097 (подстрочный 404-детект): номер с «404»
/// внутри (issues/4040) при транспортном сбое 403 rate limit — громкий
/// отказ, НЕ «не найдено» (иначе substring-подмена чужой строки —
/// ровно класс бага 1088, который канон резолва закрывает).
#[test]
fn direct_get_403_on_number_with_404_inside_is_loud_not_notfound() {
    let guards = setup_stub_gh();
    std::fs::write(guards.0.path().join("direct-403"), "").expect("маркер 403");
    let (code, stdout, stderr) = run_stub(
        &guards,
        None,
        &["task", "get", "4040", "--project", "uwuwu", "--no-body"],
    );
    let said = format!("{stdout}{stderr}");
    assert_ne!(code, Some(0), "транспортный сбой — отказ: {said}");
    assert!(
        said.contains("HTTP 403"),
        "транспортный сбой назван честно: {said}"
    );
    assert!(
        !said.contains("не найдено"),
        "403 НЕ маскируется под «не найдено» (регресс подстрочного 404-детекта): {said}"
    );
}
