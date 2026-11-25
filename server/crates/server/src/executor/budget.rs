//! Декоратор AgentSessionGateway: глобальный спавн-бюджет (директива
//! владельца 15.11). Все create_session сервера — стадии конвейера,
//! dead-respawn, крон-раны, триаж, дример — упираются в N/час;
//! UWUWU_SPAWNS_MAX_PER_HOUR=0 — kill-switch. Окно in-memory: рестарт
//! восстанавливает бюджет (шторм после рестарта держат cooldown +
//! SpawnLoop — они персистентны).

use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use uwuwu_domain::{Agent, BudgetVerdict, SpawnBudget};
use uwuwu_usecase::{AgentSessionGateway, AssistantReply, Liveness, SessionInfo, SessionSpec};
use uwuwu_usecase::{UsecaseError, UsecaseResult};

pub struct BudgetedSessions {
    inner: Arc<dyn AgentSessionGateway>,
    budget: Arc<Mutex<SpawnBudget>>,
    clock: Arc<dyn uwuwu_usecase::Clock>,
}

impl BudgetedSessions {
    /// Разделяемый бюджет (#1203): per-project хэндлы нескольких
    /// исполнителей тратят ОДИН глобальный кап — семантика бюджета
    /// не меняется с появлением маршрутизации.
    pub fn with_shared(
        inner: Arc<dyn AgentSessionGateway>,
        budget: Arc<Mutex<SpawnBudget>>,
        clock: Arc<dyn uwuwu_usecase::Clock>,
    ) -> Self {
        Self {
            inner,
            budget,
            clock,
        }
    }
}

#[async_trait]
impl AgentSessionGateway for BudgetedSessions {
    async fn create_session(&self, spec: &SessionSpec) -> UsecaseResult<String> {
        // bootstrap-сессии подготовки env не тарифицируются: логический
        // спавн = рабочая сессия (e2e 15.11: полный цикл с ре-энтри
        // съедал 10/10 bootstrap-парами и блокировал финальную стадию)
        if spec.bootstrap {
            return self.inner.create_session(spec).await;
        }
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

    /// Пин агента слота мимо бюджета (#1300): не спавн — LLM-слот не
    /// занимает, тарификации нет.
    async fn switch_agent(&self, session: &str, agent: Agent) -> UsecaseResult<()> {
        self.inner.switch_agent(session, agent).await
    }

    async fn run_command(
        &self,
        session: &str,
        name: &str,
        args: &str,
        budget_ms: u64,
    ) -> UsecaseResult<()> {
        self.inner.run_command(session, name, args, budget_ms).await
    }

    async fn liveness(&self, session: &str) -> Liveness {
        self.inner.liveness(session).await
    }

    async fn delete_session(&self, session: &str) -> UsecaseResult<()> {
        self.inner.delete_session(session).await
    }

    async fn abort_session(&self, session: &str) -> UsecaseResult<()> {
        self.inner.abort_session(session).await
    }

    fn spawn_allowed(&self) -> bool {
        // Проба без мутаций окна (SpawnBudget::peek): преемпшен #1259
        // проверяет готовность кандидата ДО гашения жертвы
        matches!(
            self.budget
                .lock()
                .expect("budget lock")
                .peek(self.clock.now_ms()),
            BudgetVerdict::Allowed
        )
    }

    async fn last_activity_ms(&self, session: &str) -> UsecaseResult<Option<u64>> {
        self.inner.last_activity_ms(session).await
    }

    /// Прямой shell без LLM-поворота (#1526): детерминированный канал,
    /// не спавн — тарификации не касается (прецедент last_activity_ms).
    async fn run_shell(
        &self,
        session: &str,
        command: &str,
        budget_ms: u64,
    ) -> UsecaseResult<Option<String>> {
        self.inner.run_shell(session, command, budget_ms).await
    }

    /// Компакт-ручки (#1221): чтение занятости и адмиссия компакта —
    /// не спавны, LLM-слот тарификации бюджета не касаются (прецедент
    /// switch_agent/abort_session).
    async fn context_usage_pct(
        &self,
        session: &str,
        model: &uwuwu_domain::ModelRef,
    ) -> UsecaseResult<Option<f64>> {
        self.inner.context_usage_pct(session, model).await
    }

    async fn compact_session(&self, session: &str) -> UsecaseResult<()> {
        self.inner.compact_session(session).await
    }

    async fn last_assistant_reply(&self, session: &str) -> UsecaseResult<Option<AssistantReply>> {
        self.inner.last_assistant_reply(session).await
    }

    async fn list_sessions(&self) -> UsecaseResult<Vec<SessionInfo>> {
        self.inner.list_sessions().await
    }

    async fn export_session(&self, session: &str) -> UsecaseResult<String> {
        self.inner.export_session(session).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting(Arc<AtomicUsize>);
    #[async_trait]
    impl AgentSessionGateway for Counting {
        async fn create_session(&self, _s: &SessionSpec) -> UsecaseResult<String> {
            Ok(format!("ses_{}", self.0.fetch_add(1, Ordering::SeqCst)))
        }
        async fn prompt(&self, _s: &str, _t: &str) -> UsecaseResult<()> {
            Ok(())
        }
        async fn run_command(&self, _s: &str, _n: &str, _a: &str, _b: u64) -> UsecaseResult<()> {
            Ok(())
        }
        async fn delete_session(&self, _s: &str) -> UsecaseResult<()> {
            Ok(())
        }
        async fn liveness(&self, _s: &str) -> Liveness {
            Liveness::Alive
        }
        async fn last_activity_ms(&self, _s: &str) -> UsecaseResult<Option<u64>> {
            Ok(None)
        }
        async fn last_assistant_reply(&self, _s: &str) -> UsecaseResult<Option<AssistantReply>> {
            Ok(None)
        }
        async fn list_sessions(&self) -> UsecaseResult<Vec<SessionInfo>> {
            Ok(vec![])
        }
        async fn export_session(&self, _s: &str) -> UsecaseResult<String> {
            Ok(String::new())
        }
    }
    struct ConstClock;
    impl uwuwu_usecase::Clock for ConstClock {
        fn utc_minute(&self) -> uwuwu_domain::UtcMinute {
            uwuwu_domain::UtcMinute {
                minute_of_day: 0,
                weekday: 1,
            }
        }
        fn now_ms(&self) -> u64 {
            1000
        }
    }

    fn spec(bootstrap: bool) -> SessionSpec {
        SessionSpec {
            custom_agent: None,
            directory: "/tmp".into(),
            title: "t".into(),
            agent: uwuwu_domain::AgentSlot::Default,
            model: uwuwu_domain::ModelRef::new("opencode-go", "hy4-flash-free"),
            variant: None,
            bootstrap,
        }
    }

    #[tokio::test]
    async fn bootstrap_sessions_bypass_budget() {
        // e2e 15.11: полный цикл с ре-энтри съедал капу bootstrap-парами;
        // логический спавн тарифа = рабочая сессия
        let inner = Arc::new(Counting(Arc::new(AtomicUsize::new(1))));
        let budgeted = BudgetedSessions::with_shared(
            inner.clone(),
            Arc::new(Mutex::new(SpawnBudget::new(2))),
            Arc::new(ConstClock),
        );
        for _ in 0..5 {
            budgeted.create_session(&spec(true)).await.unwrap(); // bootstrap — мимо кассы
        }
        budgeted.create_session(&spec(false)).await.unwrap();
        budgeted.create_session(&spec(false)).await.unwrap();
        assert!(
            budgeted.create_session(&spec(false)).await.is_err(),
            "кап 2 исчерпан"
        );
        budgeted.create_session(&spec(true)).await.unwrap(); // bootstrap по-прежнему ок
    }

    #[tokio::test]
    async fn spawn_allowed_peek_does_not_spend_budget() {
        // Проба преемпшена (#1259): готовность кандидата проверяется до
        // гашения жертвы — peek без мутаций окна, kill-switch = false
        let budgeted = BudgetedSessions::with_shared(
            Arc::new(Counting(Arc::new(AtomicUsize::new(1)))),
            Arc::new(Mutex::new(SpawnBudget::new(2))),
            Arc::new(ConstClock),
        );
        assert!(budgeted.spawn_allowed(), "свежее окно — проба true");
        // исчерпание часа: два рабочих спавна
        budgeted.create_session(&spec(false)).await.unwrap();
        budgeted.create_session(&spec(false)).await.unwrap();
        assert!(
            !budgeted.spawn_allowed(),
            "кап исчерпан — преемпшен не начинается"
        );
        // peek не тратил бюджет повторно: в окне те же две метки
        assert!(
            !budgeted.spawn_allowed(),
            "проба идемпотентна (не мутирует окно)"
        );
        // kill-switch
        let killed = BudgetedSessions::with_shared(
            Arc::new(Counting(Arc::new(AtomicUsize::new(1)))),
            Arc::new(Mutex::new(SpawnBudget::new(0))),
            Arc::new(ConstClock),
        );
        assert!(!killed.spawn_allowed(), "kill-switch — пробы нет");
    }
}
