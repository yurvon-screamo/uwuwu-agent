//! Маршрутизатор сессий «проект → executor» (#1203): per-project гейтвеи,
//! дефолт — [executor]. Union-сканы с разной семантикой отказа: strict
//! (CUP: all-or-Err — частичный список искажает подсчёт) и best-effort
//! (дример: merge успешных + warn по отказавшему; отказ всех — Err).

use crate::error::{UsecaseError, UsecaseResult};
use crate::ports::{AgentSessionGateway, SessionInfo};
use std::collections::HashMap;
use std::sync::Arc;

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

    /// Список сессий по успешным исполнителям: пост-обработка (дример)
    /// не должна останавливаться из-за одного офлайн-ноутбука. Отказ
    /// части — warn с именем исполнителя; пропущенные сессии не теряются
    /// (idle-детект и dreamer_done дообрабатывают их при его возврате).
    /// Отказ всех — Err, как сегодня при единственном исполнителе.
    pub async fn list_sessions_best_effort(&self) -> UsecaseResult<Vec<SessionInfo>> {
        let mut out = Vec::new();
        let mut failures = 0usize;
        let mut last_err: Option<UsecaseError> = None;
        for (name, gateway) in &self.all {
            match gateway.list_sessions().await {
                Ok(list) => out.extend(list),
                Err(e) => {
                    failures += 1;
                    tracing::warn!(event = "router.list_partial", executor = %name, error = %e,
                        "Union-скан: исполнитель недоступен — сессии пропущены (дообработка при возврате)");
                    last_err = Some(e);
                }
            }
        }
        if failures == self.all.len() {
            return Err(last_err.unwrap_or_else(|| {
                UsecaseError::Session("list_sessions: все исполнители недоступны".into())
            }));
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
