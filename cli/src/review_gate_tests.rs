//! Матрица гарда ревью (20261109184134-d2x7lsn): url-разбор, свёртка
//! statusCheckRollup, чистый маппинг evaluate, не-GitHub ветка gate;
//! граница «подпроцесс gh» — интеграционные тесты с фейковым gh-бинарём
//! в PATH (сценарии ответа/env — детерминированная замена живых gh-вызовов;
//! живые пробы на реальном GitHub остаются приёмочным слоем). Таймаут-ветка
//! run_gh (30с потолок) не автоматизирована — tokio::time::timeout, риск низкий.

use crate::review_gate::{
    evaluate, gate, github_owner_repo, rollup_state, url_host, RollupState, STATUS_REVIEW_RESULT,
    WorkflowProbe,
};
use serde_json::{json, Value};

fn check_run(status: &str, conclusion: &str) -> Value {
    json!({ "name": "CI", "status": status, "conclusion": conclusion })
}

fn status_context(state: &str) -> Value {
    json!({ "context": "lint", "state": state })
}

#[test]
fn url_host_parses_scheme_host_port_and_path() {
    assert_eq!(
        url_host("https://github.com/a/b/pull/1"),
        Some("github.com")
    );
    assert_eq!(url_host("http://github.com:443/a/b"), Some("github.com"));
    assert_eq!(
        url_host("https://git.true-wash.com/true-wash/deploy/pulls/8"),
        Some("git.true-wash.com")
    );
    assert_eq!(url_host("github.com/a/b/pull/1"), None, "нет scheme");
    assert_eq!(url_host(""), None);
}

#[test]
fn github_owner_repo_extracts_pull_and_pulls() {
    assert_eq!(
        github_owner_repo("https://github.com/yurvon-screamo/uwuwu-agent/pull/190"),
        Some(("yurvon-screamo".into(), "uwuwu-agent".into()))
    );
    assert_eq!(
        github_owner_repo("https://github.com/o/r/pulls/8"),
        Some(("o".into(), "r".into()))
    );
    assert_eq!(
        github_owner_repo("https://git.true-wash.com/o/r/pulls/4"),
        None,
        "не-GitHub — None"
    );
    assert_eq!(
        github_owner_repo("https://github.com/o/r"),
        None,
        "короткий путь не PR"
    );
}

#[test]
fn rollup_empty_and_null_are_empty() {
    assert_eq!(rollup_state(&json!([])), RollupState::Empty);
    assert_eq!(rollup_state(&Value::Null), RollupState::Empty);
}

/// Живая фикстура PR #190 (09.11): один CheckRun, COMPLETED + FAILURE.
#[test]
fn rollup_failed_check_run_carries_name() {
    let pr190 = json!([
        { "name": "CI", "status": "COMPLETED", "conclusion": "FAILURE",
          "workflowName": "CI" }
    ]);
    assert_eq!(rollup_state(&pr190), RollupState::Failed(vec!["CI".into()]));
}

#[test]
fn rollup_pending_and_green_check_runs() {
    assert_eq!(
        rollup_state(&json!([check_run("IN_PROGRESS", "")])),
        RollupState::Pending(vec!["CI".into()])
    );
    assert_eq!(
        rollup_state(&json!([check_run("QUEUED", "SKIPPED")])),
        RollupState::Pending(vec!["CI".into()]),
        "не COMPLETED — ещё идёт, conclusion не читается"
    );
    assert_eq!(
        rollup_state(&json!([check_run("COMPLETED", "SUCCESS")])),
        RollupState::Green
    );
    assert_eq!(
        rollup_state(&json!([
            check_run("COMPLETED", "SUCCESS"),
            check_run("COMPLETED", "SUCCESS")
        ])),
        RollupState::Green
    );
}

#[test]
fn rollup_completed_without_success_conclusion_is_failed() {
    assert_eq!(
        rollup_state(&json!([check_run("COMPLETED", "FAILURE")])),
        RollupState::Failed(vec!["CI".into()])
    );
    assert_eq!(
        rollup_state(&json!([check_run("COMPLETED", "SKIPPED")])),
        RollupState::Failed(vec!["CI".into()]),
        "strict-режим: зелёный — только SUCCESS (осознанное решение плана)"
    );
    assert_eq!(
        rollup_state(&json!([check_run("COMPLETED", "")])),
        RollupState::Failed(vec!["CI".into()]),
        "COMPLETED без conclusion — не зелёный"
    );
}

#[test]
fn rollup_status_context_branches() {
    assert_eq!(
        rollup_state(&json!([status_context("SUCCESS")])),
        RollupState::Green
    );
    assert_eq!(
        rollup_state(&json!([status_context("PENDING")])),
        RollupState::Pending(vec!["lint".into()])
    );
    assert_eq!(
        rollup_state(&json!([status_context("EXPECTED")])),
        RollupState::Pending(vec!["lint".into()])
    );
    assert_eq!(
        rollup_state(&json!([status_context("ERROR")])),
        RollupState::Failed(vec!["lint".into()])
    );
    assert_eq!(
        rollup_state(&json!([status_context("FAILURE")])),
        RollupState::Failed(vec!["lint".into()])
    );
    assert_eq!(
        rollup_state(&json!([{"name": "mystery"}])),
        RollupState::Failed(vec!["mystery".into()]),
        "неизвестная форма — не зелёный (честный отказ)"
    );
}

#[test]
fn rollup_failed_wins_over_pending() {
    let mixed = json!([
        check_run("IN_PROGRESS", ""),
        check_run("COMPLETED", "FAILURE")
    ]);
    assert_eq!(
        rollup_state(&mixed),
        RollupState::Failed(vec!["CI".into()]),
        "красный приоритетнее «идёт»: ожидание не поможет, нужен пуш фикса"
    );
}

#[test]
fn evaluate_mergeable_branches() {
    let green = RollupState::Green;
    assert!(
        matches!(
            evaluate("MERGEABLE", &green, &WorkflowProbe::Present),
            Ok(None)
        ),
        "мержабельный + зелёный — чистый проход"
    );

    let err = evaluate("CONFLICTING", &green, &WorkflowProbe::Present).unwrap_err();
    assert!(err.to_string().contains("конфликт"), "{err}");

    let err = evaluate("UNKNOWN", &green, &WorkflowProbe::Present).unwrap_err();
    assert!(err.to_string().contains("считает"), "{err}");

    let err = evaluate("WEIRD", &green, &WorkflowProbe::Present).unwrap_err();
    assert!(err.to_string().contains("WEIRD"), "{err}");
}

#[test]
fn evaluate_check_states_reject_with_recipe() {
    let failed = RollupState::Failed(vec!["CI".into(), "lint (FAILURE)".into()]);
    let err = evaluate("MERGEABLE", &failed, &WorkflowProbe::Present).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("красный"), "{msg}");
    assert!(msg.contains("CI"), "{msg}");

    let pending = RollupState::Pending(vec!["CI".into()]);
    let err = evaluate("MERGEABLE", &pending, &WorkflowProbe::Present).unwrap_err();
    assert!(err.to_string().contains("ещё идёт"), "{err}");

    assert!(
        matches!(
            evaluate("MERGEABLE", &RollupState::Green, &WorkflowProbe::Present),
            Ok(None)
        ),
        "зелёный rollup — чистый проход"
    );
}

#[test]
fn evaluate_empty_rollup_resolved_by_workflows_probe() {
    let empty = RollupState::Empty;

    let err = evaluate("MERGEABLE", &empty, &WorkflowProbe::Present).unwrap_err();
    assert!(err.to_string().contains("не стартовал"), "{err}");

    assert!(
        matches!(
            evaluate("MERGEABLE", &empty, &WorkflowProbe::Absent),
            Ok(Some(_))
        ),
        "репо без CI — проход с предупреждением"
    );

    let err = evaluate("MERGEABLE", &empty, &WorkflowProbe::Failed).unwrap_err();
    assert!(err.to_string().contains("CI-workflows"), "{err}");
}

#[tokio::test]
async fn gate_passes_non_github_with_warning() {
    gate("https://git.true-wash.com/true-wash/detector/pulls/4")
        .await
        .expect("не-GitHub проходит без gh-вызова");
    gate("http://gitea.local/origa/pull/9")
        .await
        .expect("произвольный http-хост проходит");
}

/// Связка знания: цель гарда обязана жить в реестре канона статусов.
/// Переименование в TASK_STATUSES без синхрона здесь — громкий отказ теста,
/// не молчаливое отключение гарда (ревью Low №1).
#[test]
fn review_result_status_is_registered_in_canon() {
    assert!(
        crate::siyuan_task::TASK_STATUSES.contains(&STATUS_REVIEW_RESULT),
        "«{STATUS_REVIEW_RESULT}» вне канона TASK_STATUSES — гард молча перестанет \
         применяться; синхронизируй константу с реестром"
    );
}

// ---- интеграция: граница «подпроцесс gh» — фейковый gh в PATH ----

use std::env::{set_var, var};
use tempfile::TempDir;

const GH_PR_URL: &str = "https://github.com/yurvon-screamo/uwuwu-agent/pull/190";

/// Живой формат ответа `gh pr view --json mergeable,statusCheckRollup`
/// строкой для GH_FAKE_VIEW_JSON.
pub(crate) fn pr_view_json(mergeable: &str, status: &str, conclusion: &str) -> String {
    json!({
        "mergeable": mergeable,
        "statusCheckRollup": [{ "name": "CI", "status": status, "conclusion": conclusion }]
    })
    .to_string()
}

/// Фейковый gh: bash-скрипт в tempdir + PATH-подмена (serial-тесты).
/// `pr view` печатает GH_FAKE_VIEW_JSON (exit GH_FAKE_VIEW_EXIT, дефолт 0);
/// `api` печатает GH_FAKE_API_STDOUT (exit 0) либо GH_FAKE_API_STDERR в
/// stderr (exit GH_FAKE_API_EXIT, дефолт 1). Каждый тест ставит свои
/// GH_FAKE_* ДО install().
pub(crate) struct FakeGh {
    _dir: TempDir,
}

impl FakeGh {
    pub(crate) fn install() -> Self {
        let dir = TempDir::new().expect("tempdir под фейковый gh");
        let script = dir.path().join("gh");
        std::fs::write(
            &script,
            r#"#!/usr/bin/env bash
case "$1" in
  pr)
    printf '%s' "${GH_FAKE_VIEW_STDERR:-}" >&2
    printf '%s' "$GH_FAKE_VIEW_JSON"
    exit "${GH_FAKE_VIEW_EXIT:-0}"
    ;;
  api)
    if [ -n "${GH_FAKE_API_STDOUT+x}" ]; then
      printf '%s' "$GH_FAKE_API_STDOUT"
      exit 0
    fi
    printf '%s' "$GH_FAKE_API_STDERR" >&2
    exit "${GH_FAKE_API_EXIT:-1}"
    ;;
  *)
    echo "fake gh: unexpected call: $*" >&2
    exit 99
    ;;
esac
"#,
        )
        .expect("скрипт фейкового gh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("chmod фейкового gh");
        }
        let old_path = var("PATH").unwrap_or_default();
        set_var("PATH", format!("{}:{old_path}", dir.path().display()));
        Self { _dir: dir }
    }
}

impl Drop for FakeGh {
    fn drop(&mut self) {
        // PATH-гигиена: убрать путь (ещё живого, но умирающего) tempdir;
        // GH_FAKE_* каждый serial-тест ставит сам перед install().
        let cleaned = var("PATH")
            .unwrap_or_default()
            .split(':')
            .filter(|p| Some(*p) != self._dir.path().to_str())
            .collect::<Vec<_>>()
            .join(":");
        set_var("PATH", cleaned);
    }
}

/// Полная сцена ответа `pr view` (serial-тесты шарят env — сбрасываем всё):
/// JSON + exit 0; api-ветка в дефолт (stdout не задан, exit 1, пустой stderr).
pub(crate) fn gh_env(view_json: &str) {
    set_var("GH_FAKE_VIEW_JSON", view_json);
    set_var("GH_FAKE_VIEW_EXIT", "0");
    set_var("GH_FAKE_VIEW_STDERR", "");
    std::env::remove_var("GH_FAKE_API_STDOUT");
    set_var("GH_FAKE_API_EXIT", "1");
    set_var("GH_FAKE_API_STDERR", "");
}

#[tokio::test]
#[serial_test::serial]
async fn gate_green_via_gh_passes_without_workflows_probe() {
    gh_env(&pr_view_json("MERGEABLE", "COMPLETED", "SUCCESS"));
    let _gh = FakeGh::install();
    gate(GH_PR_URL)
        .await
        .expect("зелёный rollup от gh — чистый проход, workflows-проба не зовётся");
}

#[tokio::test]
#[serial_test::serial]
async fn gate_conflicting_via_gh_rejected() {
    gh_env(&json!({ "mergeable": "CONFLICTING", "statusCheckRollup": [] }).to_string());
    let _gh = FakeGh::install();
    let err = gate(GH_PR_URL)
        .await
        .expect_err("конфликтный PR отвергается");
    assert!(format!("{err:?}").contains("конфликт"), "{err:?}");
}

#[tokio::test]
#[serial_test::serial]
async fn gate_unknown_mergeable_via_gh_rejected() {
    gh_env(&json!({ "mergeable": "UNKNOWN", "statusCheckRollup": [] }).to_string());
    let _gh = FakeGh::install();
    let err = gate(GH_PR_URL).await.expect_err("UNKNOWN отвергается");
    assert!(format!("{err:?}").contains("считает"), "{err:?}");
}

#[tokio::test]
#[serial_test::serial]
async fn gate_pending_via_gh_rejected_with_check_name() {
    gh_env(&pr_view_json("MERGEABLE", "IN_PROGRESS", ""));
    let _gh = FakeGh::install();
    let err = gate(GH_PR_URL).await.expect_err("pending-PR отвергается");
    let msg = format!("{err:?}");
    assert!(msg.contains("ещё идёт"), "{msg}");
    assert!(msg.contains("CI"), "имя чека в отказе: {msg}");
}

#[tokio::test]
#[serial_test::serial]
async fn gate_failed_via_gh_rejected_with_check_name() {
    gh_env(&pr_view_json("MERGEABLE", "COMPLETED", "FAILURE"));
    let _gh = FakeGh::install();
    let err = gate(GH_PR_URL).await.expect_err("красный PR отвергается");
    let msg = format!("{err:?}");
    assert!(msg.contains("красный"), "{msg}");
    assert!(msg.contains("CI"), "имя чека в отказе: {msg}");
}

#[tokio::test]
#[serial_test::serial]
async fn gate_empty_rollup_workflows_present_rejected() {
    gh_env(&json!({ "mergeable": "MERGEABLE", "statusCheckRollup": [] }).to_string());
    set_var(
        "GH_FAKE_API_STDOUT",
        json!({ "workflows": [ { "id": "w1" } ] }).to_string(),
    );
    let _gh = FakeGh::install();
    let err = gate(GH_PR_URL)
        .await
        .expect_err("пустой rollup при живых workflows отвергается");
    assert!(format!("{err:?}").contains("не стартовал"), "{err:?}");
}

#[tokio::test]
#[serial_test::serial]
async fn gate_empty_rollup_actions_disabled_passes() {
    gh_env(&json!({ "mergeable": "MERGEABLE", "statusCheckRollup": [] }).to_string());
    set_var("GH_FAKE_API_STDERR", "gh: HTTP 403: Actions are disabled");
    let _gh = FakeGh::install();
    gate(GH_PR_URL)
        .await
        .expect("403 workflows-пробы = репо без CI — проход (предупреждение в stdout)");
}

#[tokio::test]
#[serial_test::serial]
async fn gate_empty_rollup_api_error_rejected() {
    gh_env(&json!({ "mergeable": "MERGEABLE", "statusCheckRollup": [] }).to_string());
    set_var("GH_FAKE_API_STDERR", "gh: server error (502)");
    let _gh = FakeGh::install();
    let err = gate(GH_PR_URL)
        .await
        .expect_err("сбой workflows-пробы — громкий отказ");
    assert!(format!("{err:?}").contains("CI-workflows"), "{err:?}");
}

#[tokio::test]
#[serial_test::serial]
async fn gate_gh_failure_is_loud_rejection() {
    gh_env("");
    set_var("GH_FAKE_VIEW_EXIT", "1");
    set_var("GH_FAKE_VIEW_STDERR", "gh: network down");
    set_var("GH_FAKE_API_STDERR", "gh: network down");
    let _gh = FakeGh::install();
    let err = gate(GH_PR_URL).await.expect_err("сбой gh — отказ перевода");
    let msg = format!("{err:?}");
    assert!(msg.contains("gh pr не удался"), "{msg}");
    assert!(msg.contains("network down"), "stderr gh в отказе: {msg}");
}

#[tokio::test]
#[serial_test::serial]
async fn gate_gh_non_json_is_loud_rejection() {
    gh_env("not a json at all");
    let _gh = FakeGh::install();
    let err = gate(GH_PR_URL)
        .await
        .expect_err("не-JSON ответ gh — громкий отказ");
    assert!(format!("{err:?}").contains("не-JSON"), "{err:?}");
}
