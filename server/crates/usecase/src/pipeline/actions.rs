//! Действия тика: спавн стадии (bootstrap /command → сессия в env),
//! wakeup по свежим комментам, автодон (проба PR → мерж/Done/bounce).

use crate::error::UsecaseResult;
use crate::naming;
use crate::pipeline::tick::{BoardPlan, TickUseCase};
use crate::ports::{Liveness, SessionSpec};
use uwuwu_domain::{Agent, AgentSlot, LoopOutcome, Slot, SpawnLoop, Status, Task};

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
        let env_repo = format!("{}/{}", board.env_root, task.number());
        let title = format!("#{} {}", task.number(), task.title());
        let bootstrap = self
            .sessions
            .create_session(&SessionSpec {
                directory: board.checkout.clone(),
                title: format!("bootstrap {title}"),
                agent: AgentSlot::Default,
                model: self.spawn_model.clone(),
            })
            .await?;
        // любой сбой после создания bootstrap гасит его (нет утечки сессий)
        let result = self
            .spawn_stage_inner(&bootstrap, &env_repo, board, task, slot, &title)
            .await;
        let _ = self.sessions.delete_session(&bootstrap).await;
        result
    }

    async fn spawn_stage_inner(
        &self,
        bootstrap: &str,
        env_repo: &str,
        board: &BoardPlan,
        task: &Task,
        slot: Slot,
        title: &str,
    ) -> UsecaseResult<String> {
        self.sessions
            .run_command(bootstrap, "task-env", &task.number().to_string())
            .await?;
        let work = self
            .sessions
            .create_session(&SessionSpec {
                directory: env_repo.to_string(),
                title: title.to_string(),
                agent: AgentSlot::of(Agent::of_slot(slot)),
                model: self.spawn_model.clone(),
            })
            .await?;
        let prompt = spawn_prompt(board, task, slot);
        self.sessions.prompt(&work, &prompt).await?;
        tracing::info!(event = "spawn.done", number = task.number(), slot = ?slot,
            session = %work, env = %env_repo, "Spawn done");
        Ok(work)
    }

    /// Wakeup: мёртвая сессия — респавн (dead-детект ПЕРВЫМ: даже без свежих
    /// комментов — строка не висит, DS «dead-детект решает: будить или
    /// респавнить»); живая — будим по свежим комментам (новее активности).
    pub async fn wakeup_if_fresh(&self, board: &BoardPlan, task: &Task) -> UsecaseResult<bool> {
        let Some(slot) = task.stage_slot() else {
            return Ok(false);
        };
        let Some(session) = task.session_of(slot) else {
            return Ok(false);
        };
        // dead-детект до чтения комментов: дедуп/пустой журнал не должны
        // прятать мёртвую сессию стадии (находка мок-стенда)
        if !matches!(self.sessions.liveness(session).await, Liveness::Alive) {
            return self.respawn_dead_stage(board, task, slot, session).await;
        }
        let comments = self.board.latest_comments(task.number()).await?;
        let Some(last) = comments.last() else {
            return Ok(false);
        };
        let seen = self.state.last_comment_id(&board.board, task.number());
        if seen.as_deref() == Some(last.id.as_str()) {
            return Ok(false);
        }
        {
            let activity = self.sessions.last_activity_ms(session).await?;
            let fresh = last.created_ms > activity.unwrap_or(0);
            if fresh {
                self.sessions
                    .prompt(
                        session,
                        &format!(
                            "Wakeup: новый коммент в задаче #{}:\n\n{}",
                            task.number(),
                            last.body
                        ),
                    )
                    .await?;
                tracing::info!(
                    event = "wakeup.delivered",
                    number = task.number(),
                    "Wakeup delivered"
                );
            }
            self.state
                .set_last_comment_id(&board.board, task.number(), &last.id);
            Ok(fresh)
        }
    }

    /// Респавн мёртвой сессии стадии через луп-гвард (attention-механика:
    /// респавн = повтор слота при неизменном статусе; >2 → Needs attention).
    async fn respawn_dead_stage(
        &self,
        board: &BoardPlan,
        task: &Task,
        slot: Slot,
        session: &str,
    ) -> UsecaseResult<bool> {
        tracing::warn!(event = "wakeup.dead_session", number = task.number(),
            session = %session, "Stage session dead: respawn через луп-гвард");
        let mut guard = self
            .state
            .spawn_loop(&board.board, task.number(), slot)
            .map(|(status, repeats)| {
                let mut g = SpawnLoop::new(status);
                for _ in 0..repeats {
                    g.register_spawn(status);
                }
                g
            })
            .unwrap_or_else(|| SpawnLoop::new(task.status()));
        if guard.register_spawn(task.status()) == LoopOutcome::Escalate {
            tracing::warn!(
                event = "respawn.loop_escalated",
                number = task.number(),
                slot = naming::slot_key(slot),
                "Dead-respawn loop >2: Needs attention"
            );
            self.board
                .set_status(&board.board, task.number(), Status::NeedsAttention)
                .await?;
            self.board
                .post_comment(
                    task.number(),
                    &format!(
                        "⚠︎ Респавн-петля: сессия стадии `{}` умирает и респавнится \
                         >2 раз без прогресса статуса ({:?}). Разбор владельца.",
                        naming::slot_key(slot),
                        task.status()
                    ),
                )
                .await?;
            return Ok(false);
        }
        self.state.set_spawn_loop(
            &board.board,
            task.number(),
            slot,
            guard.status(),
            guard.repeats(),
        );
        let fresh = self.spawn_stage(board, task, slot).await?;
        self.board
            .set_session(&board.board, task.number(), slot, &fresh)
            .await?;
        Ok(false)
    }

    /// Автодон: проба PR → мерж/Done/bounce. Без PR и без ветки — Done сразу
    /// (docs-only); ветка без PR — возврат в работу (PR ещё не создан).
    pub async fn autodone(&self, board: &BoardPlan, task: &Task) -> UsecaseResult<()> {
        use crate::ports::PrState;
        let state = self
            .prs
            .probe(
                &board.repo,
                task.pr().map(|u| u.as_str()),
                task.branch().map(|b| b.as_str()),
            )
            .await?;
        if state == PrState::Pending {
            tracing::info!(
                event = "autodone.pending",
                number = task.number(),
                "Autodone: PR pending (mergeable/чекси считаются) — ждём следующего тика"
            );
            return Ok(());
        }
        match state {
            PrState::Pending => unreachable!("pending обработан выше"),
            PrState::Merged => self.finish_done(board, task, "PR смержен").await?,
            PrState::Missing if task.branch().is_none() => {
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
                let pr = task.pr().map(|u| u.as_str()).unwrap_or_default();
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
            .set_status(&board.board, task.number(), Status::Done)
            .await?;
        self.board.close_issue(task.number()).await?;
        tracing::info!(
            event = "autodone.done",
            number = task.number(),
            why,
            "Autodone: done"
        );
        Ok(())
    }

    async fn bounce(&self, board: &BoardPlan, task: &Task, why: &str) -> UsecaseResult<()> {
        self.board
            .set_status(&board.board, task.number(), Status::InProgress)
            .await?;
        self.board
            .post_comment(
                task.number(),
                &format!("Автодон: возврат в работу — {why}."),
            )
            .await?;
        tracing::info!(
            event = "autodone.bounce",
            number = task.number(),
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
         \n\
         Платформа — через MCP-инструменты сервера uwuwu (Code Mode: tools.uwuwu.*):\n\
         - тело задачи и комменты: uwuwu_task_get {{number: {number}}}\n\
         - персона владельца (читай первой): uwuwu_person_show\n\
         - канон-доки проекта: uwuwu_docs_read {{slug: \"Projects/{project}/README\"}},\n\
           Design Spec (Projects/{project}/Design Spec; нет — штатный skip), Деплоймент\n\
         - вести задачу: uwuwu_task_edit / task_comment / task_claim {{session: свой ses_…}}\n\
         \n\
         Работай по канону AGENTS.md; доклады и вопросы — комментом (uwuwu_task_comment).",
        number = task.number(),
        title = task.title(),
        board = board.board,
        project = board.project,
    )
}
