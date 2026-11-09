//! Матрица гарда ревью (20261109184134-d2x7lsn): url-разбор, свёртка
//! statusCheckRollup, чистый маппинг evaluate, не-GitHub ветка gate.
//! Ветви gate с вызовами gh юнитами не покрываются — живые пробы на
//! собственной строке задачи (см. Проверка в теле).

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
