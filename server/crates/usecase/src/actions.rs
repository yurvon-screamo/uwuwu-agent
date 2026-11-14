//! Действия тика: спавн стадии (bootstrap /command → сессия в env),
//! wakeup по свежим комментам, автодон (проба PR → мерж/Done/bounce).

use crate::error::UsecaseResult;
use crate::ports::{Liveness, SessionSpec};
use crate::tick::{BoardPlan, TickUseCase};
use uwuwu_domain::{Slot, Status, Task};

/// Агент опенкода для слота стадии (методички agents/*.md).
fn agent_of(slot: Slot) -> Option<&'static str> {
    match slot {
        Slot::Design => None,
        Slot::Dev => None,
        Slot::PlanReview => Some("plan-reviewer"),
        Slot::ResultReview => Some("code-quality-reviewer"),
        Slot::Test => Some("tester"),
    }
}

impl TickUseCase {
    /// Спавн агента стадии: bootstrap-сессия в мастер-чекауте готовит env
    /// детерминированной slash-командой (task-env, shell-блоки без LLM),
    /// рабочая сессия стартует прямо в ворктри, bootstrap гасится.
    pub async fn spawn_stage(
        &self,
        board: &BoardPlan,
        task: &Task,
        slot: Slot,
    ) -> UsecaseResult<String> {
        let env_repo = format!("{}/{}", board.env_root, task.number);
        let title = format!("#{} {}", task.number, task.title);
        let bootstrap = self
            .sessions
            .create_session(&SessionSpec {
                directory: board.checkout.clone(),
                title: format!("bootstrap {title}"),
                agent: None,
                model: self.spawn_model.clone(),
            })
            .await?;
        self.sessions
            .run_command(&bootstrap, "task-env", &task.number.to_string())
            .await?;
        let work = self
            .sessions
            .create_session(&SessionSpec {
                directory: env_repo.clone(),
                title,
                agent: agent_of(slot).map(String::from),
                model: self.spawn_model.clone(),
            })
            .await?;
        let prompt = spawn_prompt(board, task, slot);
        self.sessions.prompt(&work, &prompt).await?;
        self.sessions.delete_session(&bootstrap).await?;
        tracing::info!(event = "spawn.done", number = task.number, slot = ?slot,
            session = %work, env = %env_repo, "Spawn done");
        Ok(work)
    }

    /// Wakeup: свежие комменты (новее последней активности сессии) будят
    /// сессию стадии; мёртвая сессия — респавн (переназначение слота).
    pub async fn wakeup_if_fresh(&self, board: &BoardPlan, task: &Task) -> UsecaseResult<bool> {
        let Some(slot) = task.stage_slot() else {
            return Ok(false);
        };
        let Some(session) = task.session_of(slot) else {
            return Ok(false);
        };
        let comments = self.board.latest_comments(task.number).await?;
        let Some(last) = comments.last() else {
            return Ok(false);
        };
        let seen = self.state.last_comment_id(&board.board, task.number);
        if seen.as_deref() == Some(last.id.as_str()) {
            return Ok(false);
        }
        match self.sessions.liveness(session).await {
            Liveness::Alive => {
                let activity = self.sessions.last_activity_ms(session).await?;
                let fresh = last.created_ms > activity.unwrap_or(0);
                if fresh {
                    self.sessions
                        .prompt(
                            session,
                            &format!(
                                "Wakeup: новый коммент в задаче #{}:\n\n{}",
                                task.number, last.body
                            ),
                        )
                        .await?;
                    tracing::info!(
                        event = "wakeup.delivered",
                        number = task.number,
                        "Wakeup delivered"
                    );
                }
                self.state
                    .set_last_comment_id(&board.board, task.number, &last.id);
                Ok(fresh)
            }
            Liveness::Dead | Liveness::Unknown => {
                tracing::warn!(event = "wakeup.dead_session", number = task.number,
                    session = %session, "Stage session dead: respawn on next tick");
                Ok(false)
            }
        }
    }

    /// Автодон: проба PR → мерж/Done/bounce. Без PR и без ветки — Done сразу
    /// (docs-only); ветка без PR — возврат в работу (PR ещё не создан).
    pub async fn autodone(&self, board: &BoardPlan, task: &Task) -> UsecaseResult<()> {
        use crate::ports::PrState;
        let state = self
            .prs
            .probe(&board.repo, task.pr.as_deref(), task.branch.as_deref())
            .await?;
        match state {
            PrState::Merged => self.finish_done(board, task, "PR смержен").await?,
            PrState::Missing if task.branch.is_none() => {
                self.finish_done(board, task, "задача без PR (docs-only)")
                    .await?
            }
            PrState::Missing => self.bounce(board, task, "ветка есть, PR не найден").await?,
            PrState::Closed => self.bounce(board, task, "PR закрыт без мержа").await?,
            PrState::Open {
                mergeable: false, ..
            } => self.bounce(board, task, "PR в конфликте с master").await?,
            PrState::Open {
                ci_green: false, ..
            } => self.bounce(board, task, "красный CI у PR").await?,
            PrState::Open {
                mergeable: true,
                ci_green: true,
            } => {
                let pr = task.pr.as_deref().unwrap_or_default();
                match self.prs.merge(&board.repo, pr).await {
                    Ok(()) => {
                        self.finish_done(board, task, "PR смержен автодоном")
                            .await?
                    }
                    Err(e) => {
                        self.bounce(board, task, &format!("мерж не прошёл: {e}"))
                            .await?
                    }
                }
            }
        }
        Ok(())
    }

    async fn finish_done(&self, board: &BoardPlan, task: &Task, why: &str) -> UsecaseResult<()> {
        self.board
            .set_status(&board.board, task.number, Status::Done)
            .await?;
        self.board.close_issue(task.number).await?;
        tracing::info!(
            event = "autodone.done",
            number = task.number,
            why,
            "Autodone: done"
        );
        Ok(())
    }

    async fn bounce(&self, board: &BoardPlan, task: &Task, why: &str) -> UsecaseResult<()> {
        self.board
            .set_status(&board.board, task.number, Status::InProgress)
            .await?;
        self.board
            .post_comment(task.number, &format!("Автодон: возврат в работу — {why}."))
            .await?;
        tracing::info!(
            event = "autodone.bounce",
            number = task.number,
            why,
            "Autodone: bounce"
        );
        Ok(())
    }
}

fn spawn_prompt(board: &BoardPlan, task: &Task, slot: Slot) -> String {
    let stage = match slot {
        Slot::Design => "стадия утряски: изучи постановку, уточни у владельца, собери план",
        Slot::Dev => "реализация по утверждённому плану; веди задачу по канону AGENTS.md",
        Slot::PlanReview => "ревью плана: парафриз/zoom-out/readiness, вердикт комментом",
        Slot::ResultReview => "ревью результата: код/CI/отчёт, вердикт комментом",
        Slot::Test => "тестирование: качество фичи, вердикт pass/fail с шагами",
    };
    format!(
        "Задача #{number} «{title}» (доска «{board}», проект {project}).\n\
         Ты — агент конвейера uwuwu (методичка репо). {stage}.\n\
         Контекст задачи: тело issue #{number} в uwuwu-hub (uwuwu-cli task get {number}).\n\
         Работай по канону AGENTS.md; доклады и вопросы — комментом в задачу.",
        number = task.number,
        title = task.title,
        board = board.board,
        project = board.project,
    )
}
