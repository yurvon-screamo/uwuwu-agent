//! Юнит-тесты роутера «проект → executor» на фейках гейтвея.

use crate::error::UsecaseResult;
use crate::ports::{AgentSessionGateway, AssistantReply, Liveness, SessionInfo, SessionSpec};
use crate::router::SessionRouter;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

/// Фейк исполнителя: create_session метит id именем (доказательство
/// маршрута), list/export — настраиваемые сценарии.
struct FakeGw {
    name: &'static str,
    list: Option<Vec<SessionInfo>>,
    exports: HashMap<String, String>,
}

impl FakeGw {
    fn serving(name: &'static str, ids: &[&str]) -> Self {
        Self {
            name,
            list: Some(
                ids.iter()
                    .map(|id| SessionInfo {
                        id: (*id).to_string(),
                        agent: String::new(),
                        updated_ms: 0,
                        outcome: String::new(),
                        total_tokens: 0,
                        busy: false,
                    })
                    .collect(),
            ),
            exports: ids
                .iter()
                .map(|id| ((*id).to_string(), format!("export-of-{id}")))
                .collect(),
        }
    }

    fn failing(name: &'static str) -> Self {
        Self {
            name,
            list: None,
            exports: HashMap::new(),
        }
    }
}

#[async_trait]
impl AgentSessionGateway for FakeGw {
    async fn create_session(&self, _spec: &SessionSpec) -> UsecaseResult<String> {
        Ok(format!("ses-on-{}", self.name))
    }
    async fn prompt(&self, _s: &str, _t: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn run_command(&self, _s: &str, _n: &str, _a: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn liveness(&self, _s: &str) -> Liveness {
        Liveness::Alive
    }
    async fn delete_session(&self, _s: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn last_activity_ms(&self, _s: &str) -> UsecaseResult<Option<u64>> {
        Ok(None)
    }
    async fn last_assistant_reply(&self, _s: &str) -> UsecaseResult<Option<AssistantReply>> {
        Ok(None)
    }
    async fn list_sessions(&self) -> UsecaseResult<Vec<SessionInfo>> {
        match &self.list {
            Some(list) => Ok(list.clone()),
            None => Err(crate::error::UsecaseError::Session(format!(
                "исполнитель {} недоступен",
                self.name
            ))),
        }
    }
    async fn export_session(&self, s: &str) -> UsecaseResult<String> {
        self.exports
            .get(s)
            .cloned()
            .ok_or_else(|| crate::error::UsecaseError::Session("нет сессии".into()))
    }
}

fn gw(value: FakeGw) -> Arc<dyn AgentSessionGateway> {
    Arc::new(value)
}

#[tokio::test]
async fn project_routes_to_named_executor() {
    let router = SessionRouter::new(
        "kisa",
        gw(FakeGw::serving("kisa", &[])),
        vec![("redmi".into(), gw(FakeGw::serving("redmi", &[])))],
        HashMap::from([("1xgames".to_string(), gw(FakeGw::serving("redmi", &[])))]),
    );
    let id = router
        .project("1xgames")
        .create_session(&spec())
        .await
        .expect("спавн");
    assert_eq!(id, "ses-on-redmi", "проект уходит на свой исполнитель");
}

#[tokio::test]
async fn unknown_project_falls_back_to_default() {
    let router = SessionRouter::new(
        "kisa",
        gw(FakeGw::serving("kisa", &[])),
        vec![("redmi".into(), gw(FakeGw::serving("redmi", &[])))],
        HashMap::from([("1xgames".to_string(), gw(FakeGw::serving("redmi", &[])))]),
    );
    let id = router
        .project("нет-такого")
        .create_session(&spec())
        .await
        .expect("спавн");
    assert_eq!(id, "ses-on-kisa");
}

#[tokio::test]
async fn strict_union_merges_all_executors() {
    let router = SessionRouter::new(
        "kisa",
        gw(FakeGw::serving("kisa", &["ses-a"])),
        vec![("redmi".into(), gw(FakeGw::serving("redmi", &["ses-b"])))],
        HashMap::new(),
    );
    let list = router.list_sessions_strict().await.expect("merge");
    let mut ids: Vec<&str> = list.iter().map(|s| s.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, vec!["ses-a", "ses-b"]);
}

#[tokio::test]
async fn strict_union_fails_when_any_executor_down() {
    let router = SessionRouter::new(
        "kisa",
        gw(FakeGw::serving("kisa", &["ses-a"])),
        vec![("redmi".into(), gw(FakeGw::failing("redmi")))],
        HashMap::new(),
    );
    let err = router.list_sessions_strict().await.expect_err("all-or-Err");
    assert!(err.to_string().contains("redmi"), "{err}");
}

#[tokio::test]
async fn best_effort_merges_available_executors() {
    let router = SessionRouter::new(
        "kisa",
        gw(FakeGw::serving("kisa", &["ses-a"])),
        vec![("redmi".into(), gw(FakeGw::failing("redmi")))],
        HashMap::new(),
    );
    let list = router
        .list_sessions_best_effort()
        .await
        .expect("частичный список — Ok");
    assert_eq!(list.len(), 1, "только сессии доступного исполнителя");
    assert_eq!(list[0].id, "ses-a");
}

#[tokio::test]
async fn best_effort_fails_when_all_executors_down() {
    let router = SessionRouter::new(
        "kisa",
        gw(FakeGw::failing("kisa")),
        vec![("redmi".into(), gw(FakeGw::failing("redmi")))],
        HashMap::new(),
    );
    let err = router
        .list_sessions_best_effort()
        .await
        .expect_err("все вниз — Err как при одном исполнителе");
    assert!(err.to_string().contains("недоступен"), "{err}");
}

#[tokio::test]
async fn export_falls_over_to_other_executor() {
    let router = SessionRouter::new(
        "kisa",
        gw(FakeGw::serving("kisa", &["ses-a"])),
        vec![("redmi".into(), gw(FakeGw::serving("redmi", &["ses-b"])))],
        HashMap::new(),
    );
    let text = router
        .export_session_any("ses-b")
        .await
        .expect("fan-out находит сессию на втором исполнителе");
    assert_eq!(text, "export-of-ses-b");
}

#[tokio::test]
async fn export_fails_when_session_nowhere_found() {
    let router = SessionRouter::new(
        "kisa",
        gw(FakeGw::serving("kisa", &["ses-a"])),
        vec![("redmi".into(), gw(FakeGw::serving("redmi", &["ses-b"])))],
        HashMap::new(),
    );
    let err = router
        .export_session_any("ses-нет")
        .await
        .expect_err("нет нигде — Err перечисляет причины каждого исполнителя");
    let err = err.to_string();
    assert!(err.contains("«kisa»") && err.contains("«redmi»"), "{err}");
}

fn spec() -> SessionSpec {
    SessionSpec {
        directory: String::new(),
        title: "тест".into(),
        agent: uwuwu_domain::AgentSlot::Default,
        custom_agent: None,
        model: None,
        variant: None,
        bootstrap: false,
    }
}
