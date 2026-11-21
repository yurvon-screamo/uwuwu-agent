//! Маршрутизатор сессий «проект → executor» (#1203): per-project гейтвеи,
//! дефолт — [executor]. Union-скан reachable (#1450): сессии живых
//! исполнителей + имена отказных — отказ одного не обнуляет зрение
//! конвейера, его строки держит пер-проектное зрение/слепой hold (#1263).

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
    default_name: String,
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
            default_name: default_name.to_string(),
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

    /// Имя исполнителя проекта — ключ пер-проектного зрения тика (#1263):
    /// гард спавна строки читает список СВОЕГО исполнителя, отказ чужого
    /// зрение не гасит. Неизвестный проект — дефолтный исполнитель.
    pub fn project_executor_name(&self, project: &str) -> &str {
        let gateway = self.project(project);
        for (name, g) in &self.all {
            if Arc::ptr_eq(g, gateway) {
                return name;
            }
        }
        &self.default_name
    }

    /// Per-executor list: результат каждого исполнителя поимённо (#1263).
    /// Потребитель с пер-проектным зрением берёт результат своего
    /// исполнителя; агрегаты (CUP) сами решают union/fallback по набору.
    pub async fn list_sessions_per_executor(
        &self,
    ) -> Vec<(String, UsecaseResult<Vec<SessionInfo>>)> {
        let mut out = Vec::with_capacity(self.all.len());
        for (name, gateway) in &self.all {
            out.push((name.clone(), gateway.list_sessions().await));
        }
        out
    }

    /// Дефолтный исполнитель (дример-спавн и прочие внепроектные спавны).
    pub fn default(&self) -> &Gateway {
        &self.default
    }

    /// Union-скан живых исполнителей (#1450): сессии всех ДОСТУПНЫХ
    /// исполнителей + имена отказных. Решения по агрегату (CUP,
    /// alive-гварды) считают по тому, что видно; строки отказного
    /// исполнителя держит слепой hold (#1263) — спавн к нему невозможен,
    /// а занятые слоты мертвы вместе с ним. Полный отказ — пустой список,
    /// все имена в failed.
    pub async fn list_sessions_reachable(&self) -> (Vec<SessionInfo>, Vec<String>) {
        let mut out = Vec::new();
        let mut failed = Vec::new();
        for (name, gateway) in &self.all {
            match gateway.list_sessions().await {
                Ok(list) => out.extend(list),
                Err(e) => {
                    tracing::warn!(
                        event = "sessions.list_reachable_failed",
                        executor = %name,
                        error = %e,
                        "list_sessions исполнителя недоступен — деградация по исполнителю"
                    );
                    failed.push(name.clone());
                }
            }
        }
        (out, failed)
    }

    /// Число известных исполнителей — гард «все отказали» у потребителей
    /// reachable-скана (кроны: файр откладывается, когда файрить некому).
    pub fn executors_total(&self) -> usize {
        self.all.len()
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
