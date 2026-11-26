//! Общий компакт-проход мастер-привязок аналитика и ревьювера (#1661):
//! CUP-гейт → сортировка/дедуп проектов → привязка → liveness → разбор
//! модели → пик-гейт целевого провайдера (#1680) → пробы → вердикт →
//! адмиссия. Различия ролей заданы данными (префикс событий и наличие
//! пик-гейта), чтобы гейты не могли разойтись молча при правке одной
//! из ролей.

use crate::bots::{compact_verdict, CompactFacts, CompactPolicy};
use crate::pipeline::analyst::AnalystBoard;
use crate::ports::{Clock, Liveness, SessionInfo, TickStateStore};
use crate::router::SessionRouter;
use uwuwu_domain::{PeakWindow, SpawnModel};

/// Вход прохода. `peak: Some` — пик откладывает привязки целевого
/// провайдера (аналитик, #1680), `None` — гейта нет (ревьювер, #1634:
/// осознанное различие, не дрейф).
pub(crate) struct CompactPass<'a> {
    pub(crate) role: &'a str,
    pub(crate) event_prefix: &'a str,
    pub(crate) projects: &'a [AnalystBoard],
    pub(crate) sessions: &'a SessionRouter,
    pub(crate) state: &'a dyn TickStateStore,
    pub(crate) clock: &'a dyn Clock,
    pub(crate) scan: &'a [SessionInfo],
    pub(crate) policy: &'a CompactPolicy,
    pub(crate) cup: usize,
    pub(crate) peak: Option<&'a PeakWindow>,
}

impl CompactPass<'_> {
    pub(crate) async fn run(&self) {
        let event = |name: &str| format!("{}.{}", self.event_prefix, name);
        let active = crate::router::busy_count(self.scan);
        if active >= self.cup {
            tracing::info!(
                event = %event("compact_held_cup"),
                active,
                cap = self.cup,
                "Компакт-проход отложен: CUP cap"
            );
            return;
        }
        let now = self.clock.now_ms();
        let mut projects: Vec<&String> = self.projects.iter().map(|b| &b.project).collect();
        projects.sort();
        projects.dedup();
        for project in projects {
            let Some((session, bound_model, _)) = self.state.master_binding(self.role, project)
            else {
                continue;
            };
            let gateway = self.sessions.project(project);
            match gateway.liveness(&session).await {
                Liveness::Alive => {}
                Liveness::Dead => {
                    tracing::debug!(
                        event = %event("compact_dead_skip"),
                        project = %project,
                        "Мёртвая привязка компактом не трогается"
                    );
                    continue;
                }
                Liveness::Unknown => {
                    tracing::debug!(
                        event = %event("compact_unknown_skip"),
                        project = %project,
                        "Состояние привязки неизвестно — компакт пропущен"
                    );
                    continue;
                }
            }
            let Ok(model) = SpawnModel::parse(&bound_model) else {
                tracing::warn!(
                    event = %event("compact_model_unparseable"),
                    project = %project,
                    "Привязка с непарсящейся моделью — компакт пропущен"
                );
                continue;
            };
            if let Some(peak) = self.peak {
                if crate::spawn_models::peak_applies(&model) && peak.holds(self.clock.utc_minute())
                {
                    tracing::info!(
                        event = %event("compact_deferred_peak"),
                        project = %project,
                        "Компакт мастер-сессии отложен: пик-окно провайдера"
                    );
                    continue;
                }
            }
            let busy = self
                .scan
                .iter()
                .find(|s| s.id == session)
                .map(|s| s.busy)
                .unwrap_or(false);
            let idle_ms = match gateway.last_activity_ms(&session).await {
                Ok(ms) => ms.map(|ms| now.saturating_sub(ms)),
                Err(e) => {
                    if self.session_gone(project, &session).await {
                        tracing::warn!(event = %event("compact_session_gone"), project = %project,
                            session = %session, "Сессия потеряна исполнителем — привязка пропущена");
                        continue;
                    }
                    tracing::warn!(event = %event("compact_probe_failed"), project = %project,
                        error = %e, "Проба активности не удалась — компакт-проход завершён");
                    return;
                }
            };
            let usage_pct = match gateway.context_usage_pct(&session, &model.model).await {
                Ok(u) => u,
                Err(e) => {
                    if self.session_gone(project, &session).await {
                        tracing::warn!(event = %event("compact_session_gone"), project = %project,
                            session = %session, "Сессия потеряна исполнителем — привязка пропущена");
                        continue;
                    }
                    tracing::warn!(event = %event("compact_probe_failed"), project = %project,
                        error = %e, "Проба занятости не удалась — компакт-проход завершён");
                    return;
                }
            };
            let verdict = compact_verdict(
                &CompactFacts {
                    busy,
                    idle_ms,
                    usage_pct,
                    last_compact_ms: self.state.master_compact_ms(self.role, project),
                    now_ms: now,
                },
                self.policy,
            );
            if !verdict.fire {
                tracing::debug!(
                    event = %event("compact_skip"),
                    project = %project,
                    reason = %verdict.reason,
                    "Компакт: не стреляем"
                );
                continue;
            }
            // Whole-pass CUP-гейт выше; per-admission счётчик не нужен
            // (форма #1634: проход либо идёт целиком, либо отложен).
            // Метка ДО POST (ADR-082): коулдаун переживает рестарт сервера,
            // повторная адмиссия исключена.
            self.state.set_master_compact_ms(self.role, project, now);
            if let Err(e) = self.state.save() {
                tracing::warn!(event = %event("compact_save_failed"),
                    project = %project, error = %e, "Метка компакта не персистится");
            }
            match gateway.compact_session(&session).await {
                Ok(()) => {
                    tracing::info!(
                        event = %event("compacted"),
                        project = %project,
                        session = %session,
                        reason = %verdict.reason,
                        "Нативный compact admitted"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        event = %event("compact_failed"),
                        project = %project,
                        session = %session,
                        error = %e,
                        "Адмиссия компакта не удалась — ретрай по коулдауну"
                    );
                }
            }
        }
    }

    /// Liveness Dead — сессия потеряна исполнителем; Unknown — проба
    /// недостоверна, проход завершается (эталон — бот-супервизор).
    async fn session_gone(&self, project: &str, session: &str) -> bool {
        matches!(
            self.sessions.project(project).liveness(session).await,
            Liveness::Dead
        )
    }
}
