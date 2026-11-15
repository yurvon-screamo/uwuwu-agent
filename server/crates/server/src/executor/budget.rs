//! Декоратор AgentSessionGateway: глобальный спавн-бюджет (директива
//! владельца 15.11). Все create_session сервера — стадии конвейера,
//! dead-respawn, крон-раны, триаж, дример — упираются в N/час;
//! UWUWU_SPAWNS_MAX_PER_HOUR=0 — kill-switch. Окно in-memory: рестарт
//! восстанавливает бюджет (шторм после рестарта держат cooldown +
//! SpawnLoop — они персистентны).

use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use uwuwu_domain::{BudgetVerdict, SpawnBudget};
use uwuwu_usecase::{AgentSessionGateway, Liveness, SessionInfo, SessionSpec};
use uwuwu_usecase::{UsecaseError, UsecaseResult};

pub struct BudgetedSessions {
    inner: Arc<dyn AgentSessionGateway>,
    budget: Mutex<SpawnBudget>,
    clock: Arc<dyn uwuwu_usecase::Clock>,
}

impl BudgetedSessions {
    pub fn new(
        inner: Arc<dyn AgentSessionGateway>,
        cap_per_hour: usize,
        clock: Arc<dyn uwuwu_usecase::Clock>,
    ) -> Self {
        Self {
            inner,
            budget: Mutex::new(SpawnBudget::new(cap_per_hour)),
            clock,
        }
    }
}

#[async_trait]
impl AgentSessionGateway for BudgetedSessions {
    async fn create_session(&self, spec: &SessionSpec) -> UsecaseResult<String> {
        let now = self.clock.now_ms();
        let verdict = self.budget.lock().expect("budget lock").register(now);
        match verdict {
            BudgetVerdict::Allowed => self.inner.create_session(spec).await,
            BudgetVerdict::Exhausted { cap, used } => {
                tracing::warn!(event = "spawn.budget_exhausted",
                    cap, used, kill_switch = cap == 0,
                    title = %spec.title,
                    "Spawn budget: отказ create_session");
                Err(UsecaseError::Session(format!(
                    "спавн-бюджет исчерпан ({used}/{cap} за час{}): \
                     повтор позже",
                    if cap == 0 { ", kill-switch" } else { "" }
                )))
            }
        }
    }

    async fn prompt(&self, session: &str, text: &str) -> UsecaseResult<()> {
        self.inner.prompt(session, text).await
    }

    async fn run_command(&self, session: &str, name: &str, args: &str) -> UsecaseResult<()> {
        self.inner.run_command(session, name, args).await
    }

    async fn liveness(&self, session: &str) -> Liveness {
        self.inner.liveness(session).await
    }

    async fn delete_session(&self, session: &str) -> UsecaseResult<()> {
        self.inner.delete_session(session).await
    }

    async fn last_activity_ms(&self, session: &str) -> UsecaseResult<Option<u64>> {
        self.inner.last_activity_ms(session).await
    }

    async fn list_sessions(&self) -> UsecaseResult<Vec<SessionInfo>> {
        self.inner.list_sessions().await
    }

    async fn export_session(&self, session: &str) -> UsecaseResult<String> {
        self.inner.export_session(session).await
    }
}
