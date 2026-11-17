//! Маршрутизатор сессий «проект → executor» (#1203): per-project гейтвеи,
//! дефолт — [executor]. Union-скан strict: all-or-Err — частичный список
//! искажает busy-подсчёт CUP и alive-гварды (#1231).

use crate::error::{UsecaseError, UsecaseResult};
use crate::ports::{AgentSessionGateway, SessionInfo};
use std::collections::HashMap;
use std::sync::Arc;
use uwuwu_domain::Agent;

/// Число busy-сессий union-скана — занятые слоты единого капа (#1231).
pub fn busy_count(list: &[SessionInfo]) -> usize {
    list.iter().filter(|s| s.busy).count()
}

/// Есть ли среди busy-сессий агент данной роли: alive-гвард «≤1 живого»
/// (#1231). «Живой» = ход в полёте: outcome — атрибут последнего финала
/// и при новом ходе не очищается.
pub fn has_busy_agent(list: &[SessionInfo], agent: Agent) -> bool {
    list.iter()
        .any(|s| s.busy && crate::naming::parse_agent(&s.agent) == Some(agent))
}

/// Гейтвей одного исполнителя (OpencodeClient или бюджетный декоратор).
pub type Gateway = Arc<dyn AgentSessionGateway>;

pub struct SessionRouter {
    default: Gateway,
    /// Все исполнители (дефолт первым) с именами: union-сканы и warn-логи.
    all: Vec<(String, Gateway)>,
    by_project: HashMap<String, Gateway>,
}

impl SessionRouter {
    /// Дефолтный исполнитель + именованные + маршруты проектов.
    /// `named` — (имя, гейтвей); повторяющиеся имена схлопываются.
    pub fn new(
        default_name: &str,
        default: Gateway,
        named: Vec<(String, Gateway)>,
        by_project: HashMap<String, Gateway>,
    ) -> Self {
        let mut all = vec![(default_name.to_string(), default.clone())];
        for (name, gateway) in named {
            if name != default_name && !all.iter().any(|(n, _)| n == &name) {
                all.push((name, gateway));
            }
        }
        Self {
            default,
            all,
            by_project,
        }
    }

    /// Один исполнитель на всё: юнит-тесты и одноконфигурационный контур.
    pub fn uniform(gateway: Gateway) -> Self {
        Self::new("default", gateway, Vec::new(), HashMap::new())
    }

    /// Гейтвей исполнителя проекта; неизвестное имя — дефолтный
    /// (конфиг валидируется на старте, фолбэк — на всякий случай).
    pub fn project(&self, name: &str) -> &Gateway {
        self.by_project.get(name).unwrap_or(&self.default)
    }

    /// Дефолтный исполнитель (дример-спавн и прочие внепроектные спавны).
    pub fn default(&self) -> &Gateway {
        &self.default
    }

    /// Список сессий со ВСЕХ исполнителей, all-or-Err: потребители,
    /// принимающие решения по агрегату (CUP), частичный список не получают —
    /// он представил бы сессии офлайн-исполнителя мёртвыми → овер-спавн.
    pub async fn list_sessions_strict(&self) -> UsecaseResult<Vec<SessionInfo>> {
        let mut out = Vec::new();
        for (name, gateway) in &self.all {
            match gateway.list_sessions().await {
                Ok(list) => out.extend(list),
                Err(e) => {
                    return Err(UsecaseError::Session(format!(
                        "list_sessions: исполнитель «{name}» недоступен: {e}"
                    )))
                }
            }
        }
        Ok(out)
    }

    /// Экспорт сессии с веером по исполнителям: дефолт → остальные,
    /// первый успех выигрывает (MCP session_export без привязки к проекту).
    /// Все исполнители ответили неудачей — Err перечисляет причины КАЖДОГО
    /// (диагностика: красный дефолт не маскируется «нет сессии» на остальных).
    pub async fn export_session_any(&self, session: &str) -> UsecaseResult<String> {
        let mut failures: Vec<String> = Vec::new();
        for (name, gateway) in &self.all {
            match gateway.export_session(session).await {
                Ok(text) => return Ok(text),
                Err(e) => failures.push(format!("«{name}»: {e}")),
            }
        }
        Err(UsecaseError::Session(format!(
            "export_session {session} — исполнители: {}",
            failures.join("; ")
        )))
    }
}
