//! GitHubStore: TaskStore над uwuwu-hub (issues + Projects v2 + body
//! frontmatter). Контракт миграции — тело задачи 20261112155302-ttyp35g:
//! label `proj:<имя>` = доска проекта; Status/Запланировано — поля Project
//! v2; Branch/PR/Сессия dev/Расписание — frontmatter в body.
//!
//! Идемпотентный дубль-резолв — строго list-issues по label+тайтл
//! (консистентный REST); search API — только для инициализации списков
//! (eventual consistency индекса, вердикт ревью дельта-раунд 2).
//! Write-side операции — github_task_write.rs (единообразие с SiYuanStore).

use super::task_store::StoreResult;
use super::{Task, TaskBoard, TaskType};
use crate::store::github_gh::gh_json;
use serde_json::Value;

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

    /// Конвертация issue-API ответа (body с frontmatter) в нейтральный Task.
    pub(super) fn convert(&self, issue: &Value) -> Task {
        let front = Frontmatter::parse(issue["body"].as_str().unwrap_or(""));
        Task {
            id: issue["number"].to_string(),
            title: issue["title"].as_str().unwrap_or("").to_string(),
            status: front.get("status").unwrap_or_else(|| "Todo".into()),
            task_type: TaskType(
                issue
                    .pointer("/type/name")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_lowercase(),
            ),
            branch: front.get("branch").unwrap_or_default(),
            pr: front.get("pr").unwrap_or_default(),
            dev_session: front.get("dev-session").unwrap_or_default(),
            planned: front.get("planned").unwrap_or_default(),
            planned_ms: planned_ms(front.get("planned").as_deref()),
            schedule_cell: front.get("schedule").unwrap_or_default(),
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

    /// Issue после мутации (read-after-write контракта).
    pub(super) async fn issue_fresh(&self, number: &str) -> StoreResult<Task> {
        Ok(self.convert(
            &gh_json(
                &["api", &format!("repos/{}/issues/{number}", self.repo)],
                None,
            )
            .await?,
        ))
    }
}

/// «Запланировано» → epoch ms ("" → 0). Frontmatter несёт ISO-дату.
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

/// Body frontmatter задачи: машиночитаемые ячейки доски на GitHub
/// (Branch/PR/Сессия dev/Запланировано/Расписание/status-draft); Status —
/// поле Project v2, сюда дублируется для read-паритета list без GraphQL.
pub(crate) struct Frontmatter {
    raw_fields: Vec<(String, String)>,
    /// Тело после frontmatter-блока (или всё тело, если блока не было).
    pub rest_md: String,
}

impl Frontmatter {
    pub(crate) fn parse(body_md: &str) -> Self {
        let mut raw_fields = Vec::new();
        let mut rest_md = body_md.to_string();
        if let Some(after_open) = body_md.strip_prefix("---\n") {
            if let Some(end) = after_open.find("\n---\n") {
                let block = &after_open[..end];
                rest_md = after_open[end + 5..].to_string();
                for line in block.lines() {
                    if let Some((k, v)) = line.split_once(": ") {
                        raw_fields.push((k.trim().to_string(), v.trim().to_string()));
                    }
                }
            }
        }
        Self {
            raw_fields,
            rest_md,
        }
    }

    pub(crate) fn get(&self, key: &str) -> Option<String> {
        self.raw_fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    /// Body с заменой/вставкой ключа frontmatter (блок пересобирается,
    /// порядок полей сохраняется; тело после блока не трогается).
    pub(crate) fn with_field(&self, key: &str, value: &str) -> String {
        self.with_fields(&[(key, value)])
    }

    /// Мульти-правка frontmatter одним проходом (edit пачки ячеек).
    pub(crate) fn with_fields(&self, fields: &[(&str, &str)]) -> String {
        let mut merged: Vec<(String, String)> = self
            .raw_fields
            .iter()
            .filter(|(k, _)| !fields.iter().any(|(fk, _)| fk == k))
            .cloned()
            .collect();
        for (k, v) in fields {
            merged.push(((*k).to_string(), (*v).to_string()));
        }
        let mut out = String::from("---\n");
        for (k, v) in &merged {
            out.push_str(&format!("{k}: {v}\n"));
        }
        out.push_str("---\n");
        out.push_str(&self.rest_md);
        out
    }
}
