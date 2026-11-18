//! Общие стабы тест-фикстур поверхности MCP (tools_tests, tools_pins_tests):
//! вики/сессии/state в армах диспетчера не участвуют — no-op-реализации
//! поверх MemoryHub-носителя задач. Раньше дублировались байт-в-байт в
//! обоих тест-файлах (identical-code smell, раунд #1256).

use async_trait::async_trait;
use uwuwu_domain::{Slot, Status};
use uwuwu_usecase::{
    AgentSessionGateway, Liveness, SessionInfo, SessionSpec, TickStateStore, UsecaseResult,
    WikiRepo,
};

/// Вики в пробах диспетчера не читается: доков нет.
pub(crate) struct StubWiki;

#[async_trait]
impl WikiRepo for StubWiki {
    async fn doc_shas(&self) -> UsecaseResult<Vec<(String, String)>> {
        Ok(vec![])
    }
    async fn read_doc(&self, _slug: &str) -> UsecaseResult<Option<String>> {
        Ok(None)
    }
    async fn list_docs(&self, _prefix: &str) -> UsecaseResult<Vec<String>> {
        Ok(vec![])
    }
    async fn write_doc(&self, _: &str, _: &str, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn append_doc(&self, _: &str, _: &str, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
}

/// Сессии в пробах не вызываются: гварды армов стреляют до gateway.
pub(crate) struct StubSessions;

#[async_trait]
impl AgentSessionGateway for StubSessions {
    async fn create_session(&self, _: &SessionSpec) -> UsecaseResult<String> {
        Ok("ses_stub".into())
    }
    async fn prompt(&self, _: &str, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn run_command(&self, _: &str, _: &str, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn liveness(&self, _: &str) -> Liveness {
        Liveness::Unknown
    }
    async fn delete_session(&self, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn last_activity_ms(&self, _: &str) -> UsecaseResult<Option<u64>> {
        Ok(None)
    }
    async fn list_sessions(&self) -> UsecaseResult<Vec<SessionInfo>> {
        Ok(vec![])
    }
    async fn export_session(&self, _: &str) -> UsecaseResult<String> {
        Ok(String::new())
    }
    async fn last_assistant_reply(
        &self,
        _: &str,
    ) -> UsecaseResult<Option<uwuwu_usecase::AssistantReply>> {
        Ok(None)
    }
}

/// No-op TickStateStore: писателю нужен факт вызова mark_own_comment,
/// состояние в пробах не проверяется.
pub(crate) struct StubState;

impl TickStateStore for StubState {
    fn last_comment_id(&self, _: &str, _: u64) -> Option<String> {
        None
    }
    fn last_seen_status(&self, _: &str, _: u64) -> Option<Status> {
        None
    }
    fn set_last_seen_status(&self, _: &str, _: u64, _: Status) {}
    fn set_last_comment_id(&self, _: &str, _: u64, _: &str) {}
    fn dreamer_done(&self, _: &str) -> bool {
        false
    }
    fn mark_dreamer_done(&self, _: &str) {}
    fn dreamer_day_spawns(&self, _: &str) -> u32 {
        0
    }
    fn mark_dreamer_spawn(&self, _: &str) {}
    fn last_spawn_ms(&self, _: &str, _: u64, _: Slot) -> Option<u64> {
        None
    }
    fn mark_spawn(&self, _: &str, _: u64, _: Slot, _: u64) {}
    fn clear_spawn_ms(&self, _: &str, _: u64, _: Slot) {}
    fn last_analys_ms(&self, _: &str, _: u64) -> Option<u64> {
        None
    }
    fn mark_analys_spawn(&self, _: &str, _: u64, _: u64) {}
    fn last_analys_review_ms(&self, _: &str, _: u64) -> Option<u64> {
        None
    }
    fn mark_analys_review_spawn(&self, _: &str, _: u64, _: u64) {}
    fn analys_loop(&self, _: &str, _: u64) -> Option<(Status, u32)> {
        None
    }
    fn set_analys_loop(&self, _: &str, _: u64, _: Status, _: u32) {}
    fn clear_analys_loop(&self, _: &str, _: u64) {}
    fn analys_review_loop(&self, _: &str, _: u64) -> Option<(Status, u32)> {
        None
    }
    fn set_analys_review_loop(&self, _: &str, _: u64, _: Status, _: u32) {}
    fn clear_analys_review_loop(&self, _: &str, _: u64) {}
    fn prune_analys_guards(&self, _: &std::collections::HashSet<String>) {}
    fn stall_ping(&self, _: &str, _: u64) -> Option<(Status, u64, u32)> {
        None
    }
    fn mark_stall_ping(&self, _: &str, _: u64, _: Status, _: u64) {}
    fn clear_stall(&self, _: &str, _: u64) {}
    fn spawn_loop(&self, _: &str, _: u64, _: Slot) -> Option<(Status, u32)> {
        None
    }
    fn set_spawn_loop(&self, _: &str, _: u64, _: Slot, _: Status, _: u32) {}
    fn reset_stale_spawn_loops(&self, _: &str, _: u64, _: Status) {}
    fn own_comment(&self, _: &str) -> bool {
        false
    }
    fn mark_own_comment(&self, _: &str, _: u64) {}
    fn save(&self) -> UsecaseResult<()> {
        Ok(())
    }
}
