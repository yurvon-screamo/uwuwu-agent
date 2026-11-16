//! Аналитика (замена триажа, #1135): все строки Analys досок образуют
//! одну волну → одна fresh-сессия аналитика на волну (методичка
//! agents/analyst: sourcing, соразмерность рамок, вердикты через MCP).

use crate::error::UsecaseResult;
use crate::ports::{AgentSessionGateway, SessionSpec, TaskBoardRepo, TickStateStore};
use std::sync::Arc;
use uwuwu_domain::{Agent, AgentSlot, Status, Task};

/// Кулдаун волны по строке (анти-шторм, как у спавнов слотов).
const WAVE_COOLDOWN_MS: u64 = 15 * 60_000;
/// Цикл-гвард: три круга (волна→ревью) без выхода в Todo — тупик
/// владельцу (канон «третий круг»); четвёртая волна не спавнится.
const CYCLE_LIMIT: u32 = 3;

pub struct AnalystUseCase {
    pub board: Arc<dyn TaskBoardRepo>,
    /// Доски аналитики: (проект, доска, чекаут) — волна собирает со всех.
    pub boards: Vec<AnalystBoard>,
    pub sessions: Arc<dyn AgentSessionGateway>,
    pub state: Arc<dyn TickStateStore>,
    pub clock: Arc<dyn crate::ports::Clock>,
}

/// Доска аналитики: где искать очередь и где спавнить агента.
#[derive(Debug, Clone)]
pub struct AnalystBoard {
    pub project: String,
    pub board: String,
    pub checkout: String,
}

/// Строка вAnalys вне цикл-гварда, готовая к волне.
struct WaveRow {
    task: Task,
    board: AnalystBoard,
}

impl AnalystUseCase {
    /// Один проход: не больше одного спавна за тик — вся очередь
    /// (все доски) летит в одну сессию пачкой.
    pub async fn tick(&self) -> UsecaseResult<usize> {
        // ≤1 живого аналитика одновременно (паттерн дримера)
        if self.sessions.list_sessions().await?.iter().any(|s| {
            s.outcome.is_empty() && crate::naming::parse_agent(&s.agent) == Some(Agent::Analyst)
        }) {
            tracing::debug!(event = "analys.alive_skip", "Живой аналитик — волна ждёт");
            return Ok(0);
        }
        let now = self.clock.now_ms();
        let mut wave: Vec<WaveRow> = Vec::new();
        for tb in &self.boards {
            let rows = match self.board.read_board(&tb.board).await {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!(event = "analys.board_failed",
                        board = %tb.board, error = %e, "Аналитика: доска не прочитана");
                    continue;
                }
            };
            for t in rows {
                // цикл-гвард: счётчик строки вне цикла Analys↔Review Analys
                // устарел (строка уехала в пайп/терминал) — сброс
                if let Some((st, _)) = self.state.analys_loop(&tb.board, t.number()) {
                    if !matches!(t.status(), Status::Analys | Status::ReviewAnalys)
                        && st != t.status()
                    {
                        self.state.clear_analys_loop(&tb.board, t.number());
                    }
                }
                if t.status() != Status::Analys || t.title().is_empty() {
                    continue;
                }
                // ⏰-тайтлы — крон-семейство (остаток миграции ADR-103):
                // ран-карточки и мастера-строки мимо аналитики
                if t.title().starts_with('⏰') {
                    continue;
                }
                let fresh = self
                    .state
                    .last_analys_ms(&tb.board, t.number())
                    .map(|ts| now.saturating_sub(ts) >= WAVE_COOLDOWN_MS)
                    .unwrap_or(true);
                if !fresh {
                    continue;
                }
                let rounds = self
                    .state
                    .analys_loop(&tb.board, t.number())
                    .map(|(_, r)| r)
                    .unwrap_or(0);
                if rounds >= CYCLE_LIMIT {
                    self.escalate(tb, &t, rounds).await;
                    continue;
                }
                wave.push(WaveRow {
                    task: t,
                    board: tb.clone(),
                });
            }
        }
        if wave.is_empty() {
            return Ok(0);
        }
        // порядок пачки — приоритет очереди (high → … → пусто)
        wave.sort_by_key(|w| w.task.priority().rank());
        for w in &wave {
            let rounds = self
                .state
                .analys_loop(&w.board.board, w.task.number())
                .map(|(_, r)| r)
                .unwrap_or(0);
            self.state
                .set_analys_loop(&w.board.board, w.task.number(), Status::Analys, rounds + 1);
            self.state
                .mark_analys_spawn(&w.board.board, w.task.number(), now);
        }
        let first = &wave[0].board;
        let spec = SessionSpec {
            directory: first.checkout.clone(),
            title: "аналитика волны".into(),
            agent: AgentSlot::Role(Agent::Analyst),
            model: None,
            variant: None,
            bootstrap: false,
        };
        let session = self.sessions.create_session(&spec).await?;
        let rows: Vec<&Task> = wave.iter().map(|w| &w.task).collect();
        let prompt = analyst_prompt(&first.project, &rows);
        self.sessions.prompt(&session, &prompt).await?;
        tracing::info!(event = "analys.spawned", rows = rows.len(), session = %session,
            "Analyst wave spawned");
        Ok(1)
    }

    /// Тупик цикла: строка переведена в Needs attention + коммент владельцу.
    async fn escalate(&self, tb: &AnalystBoard, task: &Task, rounds: u32) {
        tracing::warn!(
            event = "analys.cycle_escalated",
            number = task.number(),
            rounds,
            "Цикл Analys↔Review Analys без выхода в Todo"
        );
        self.state.clear_analys_loop(&tb.board, task.number());
        if let Err(e) = self
            .board
            .set_status(&tb.board, task.number(), Status::NeedsAttention)
            .await
        {
            tracing::error!(event = "analys.escalate_failed", number = task.number(), error = %e, "Эскалация не применилась");
            return;
        }
        if let Err(e) = self
            .board
            .post_comment(
                task.number(),
                &format!(
                    "⚠︎ Цикл-гвард аналитики: {rounds} круга Analys↔Review Analys без выхода \
                     в Todo. Строка переведена в Needs attention — разбор владельца."
                ),
            )
            .await
        {
            tracing::error!(event = "analys.escalate_comment_failed", number = task.number(), error = %e, "Коммент эскалации не ушёл");
        }
    }
}

fn analyst_prompt(project: &str, rows: &[&Task]) -> String {
    let mut list = String::new();
    for t in rows {
        list.push_str(&format!("- #{} «{}»\n", t.number(), t.title()));
    }
    format!(
        "Аналитика волны (проект {project}) — строки в Analys, пачка целиком:\n\n{list}\n\
         Действуй по методичке репо agents/analyst. Через MCP uwuwu для каждой строки:\n\
         1. uwuwu_task_get {{number}} — заявка целиком; источники: Design Spec проекта,\n\
            README/Домены, ADR, дословные цитаты владельца в заявке/треде.\n\
         2. Проработка по типу (feature — US/UC/НФТ/«Как тестируем»; bug — симптом +\n\
            фрейм воспроизведения; spike/docs — гипотеза + «что считаем ответом»);\n\
            артефакт — переработанное тело (uwuwu_task_body) по TASK-TEMPLATE-костяку типа.\n\
         3. Вердикты: проработано → uwuwu_task_edit {{number, status: \"Review Analys\"}};\n\
            дубль/мусор → uwuwu_task_close {{number, not_planned: true}} с обоснованием;\n\
            реальный блокер → Needs attention; развилка не по ФТ/НФТ → 🚩-флаг в тело\n\
            (развилка А/Б + явная рекомендация), вопросов владельцу нет.\n\
         Ты — аналитик: не реализуй задачу, не пиши в Design Spec, не создавай задач."
    )
}
