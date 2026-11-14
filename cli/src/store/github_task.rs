//! GitHubStore: TaskStore над uwuwu-hub (issues + Projects v2 + глобальные
//! issue fields). Контракт миграции — тело задачи 20261112155302-ttyp35g:
//! label `proj:<имя>` = доска проекта; Status — Pv2 single-select (канбан);
//! клетки (Приоритет/Autodone/Branch/PR/Сессии/Расписание/Запланировано) —
//! глобальные issue fields, payload `issue_field_values` (ADR-098, задача
//! 1087); fm-шапка тел снесена.
//!
//! Идемпотентный дубль-резолв — list-issues по label+тайтл с паузой ≥2s
//! (ретрай после таймаута): ЖИВАЯ ПРОБА 13.11 — list GitHub имеет лаг ~2–4s
//! после POST (и с label-фильтром, и без) — search API тем более (eventual
//! consistency, вердикт ревью дельта-раунд 2). Немедленный ретрай после
//! таймаута видит пусто — пауза и повторный list обязательны.
//! Write-side операции — github_task_write.rs (единый trait-impl).

use super::task_store::StoreResult;
use super::{Task, TaskBoard, TaskType};
use crate::store::github_gh::gh_json;
use serde_json::Value;

/// Minimal percent-encoding для query-значений (colon/space/слэш).
pub(crate) fn urlencoded(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Гейт hub-режима для cmd-путей: UWUWU_HUB_REPO непуст.
pub fn hub_repo_non_empty() -> bool {
    (std::env::var("UWUWU_HUB_REPO").unwrap_or_default()).trim() != ""
}

pub struct GitHubStore {
    /// yurvon-screamo/uwuwu-hub (или sandbox для e2e).
    pub repo: String,
}

impl GitHubStore {
    pub fn new(repo: String) -> Self {
        Self { repo }
    }

    pub(super) fn label_of(&self, board: &TaskBoard) -> String {
        format!("proj:{}", board.project)
    }

    /// Конвертация issue-API ответа в нейтральный Task. Клетки — payload
    /// `issue_field_values` (норм-форма, ADR-098); Status — Pv2-джойн
    /// (`pv2_status`, "" — карточки/канбана нет) с проекцией closed→Done;
    /// fm-шапка тела мертва (снесена миграцией 1087).
    pub(super) fn convert(&self, issue: &Value, pv2_status: Option<&str>) -> Task {
        let fields = crate::store::github_issue_fields::fields_of(issue);
        let get = |key: &str| fields.get(key).cloned().unwrap_or_default();
        // state — проекция статуса: closed = Done (контракт миграции),
        // иначе Pv2-поле канбана (карточки нет → Todo-дефолт)
        let status = match issue["state"].as_str() {
            Some("closed") => "Done".to_string(),
            _ => match pv2_status {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => "Todo".to_string(),
            },
        };
        Task {
            id: issue["number"].to_string(),
            title: issue["title"].as_str().unwrap_or("").to_string(),
            status,
            task_type: TaskType(
                issue
                    .pointer("/type/name")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_lowercase(),
            ),
            branch: get(crate::store::github_issue_fields::names::BRANCH),
            pr: get(crate::store::github_issue_fields::names::PR),
            dev_session: get(crate::store::github_issue_fields::names::DEV_SESSION),
            planned: get(crate::store::github_issue_fields::names::PLANNED),
            planned_ms: planned_ms(
                fields
                    .get(crate::store::github_issue_fields::names::PLANNED)
                    .map(String::as_str),
            ),
            schedule_cell: get(crate::store::github_issue_fields::names::SCHEDULE),
            priority: get(crate::store::github_issue_fields::names::PRIORITY),
            autodone: matches!(
                fields
                    .get(crate::store::github_issue_fields::names::AUTODONE)
                    .map(String::as_str),
                Some("on") | Some("true") | Some("✓")
            ),
        }
    }

    /// Тело issue как md (read-after-write для frontmatter-правок).
    pub(super) async fn issue_body(&self, number: &str) -> StoreResult<String> {
        Ok(gh_json(
            &["api", &format!("repos/{}/issues/{number}", self.repo)],
            None,
        )
        .await?
        .get("body")
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .to_string())
    }

    /// PATCH тела issue (единая точка frontmatter-записи).
    pub(super) async fn set_issue_body(&self, number: &str, body_md: &str) -> StoreResult<()> {
        gh_json(
            &[
                "api",
                "-X",
                "PATCH",
                &format!("repos/{}/issues/{number}", self.repo),
            ],
            Some(&serde_json::json!({ "body": body_md }).to_string()),
        )
        .await?;
        Ok(())
    }

    /// Issue после мутации (read-after-write контракта). Статус — точечный
    /// Pv2-резолв (issue.projectItems → Status).
    pub(super) async fn issue_fresh(&self, number: &str) -> StoreResult<Task> {
        let pv2 = super::github_project_resolve::project_status_of(self, number)
            .await
            .unwrap_or(None);
        Ok(self.convert(
            &gh_json(
                &["api", &format!("repos/{}/issues/{number}", self.repo)],
                None,
            )
            .await?,
            pv2.as_deref(),
        ))
    }
}

/// «Запланировано» → epoch ms ("" → 0). Поле несёт ISO-дату.
fn planned_ms(planned: Option<&str>) -> i64 {
    let Some(s) = planned else { return 0 };
    if s.is_empty() {
        return 0;
    }
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| {
            d.and_hms_opt(0, 0, 0)
                .map(|dt| dt.and_utc().timestamp_millis())
        })
        .unwrap_or(0)
}
