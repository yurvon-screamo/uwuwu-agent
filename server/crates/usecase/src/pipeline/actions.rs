//! Действия тика: спавн стадии (bootstrap /command → сессия в env),
//! wakeup по свежим комментам, автодон (проба PR → мерж/Done/bounce).

use crate::error::{UsecaseError, UsecaseResult};
use crate::naming;
use crate::pipeline::tick::{BoardPlan, TickUseCase};
use crate::ports::{CommentRef, Liveness, SessionSpec};
use crate::router::Gateway;
use uwuwu_domain::{
    gate, Action, Agent, AgentSlot, Gate, LoopOutcome, Priority, Slot, SpawnLoop, SpawnPolicy,
    Status, Task,
};

/// Итог попытки будильника/респавна stage-сессии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeAct {
    /// Живой сессии доставлен свежий коммент.
    Woke,
    /// Мёртвая сессия зареспавнена (фактический спавн).
    Respawned,
    /// Запуск агента отложен политикой (пик/CUP) — сталл-пасс не нужен.
    Deferred,
    /// Нечего будить: тишина, коммент уже отработан или луп-эскалация.
    None,
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
        let sessions = self.sessions.project(&board.project);
        let env_repo = format!("{}/{}", board.env_root, task.number());
        let title = format!("#{} {}", task.number(), task.title());
        let bootstrap = sessions
            .create_session(&SessionSpec {
                directory: board.checkout.clone(),
                title: format!("bootstrap {title}"),
                custom_agent: None,
                agent: AgentSlot::Default,
                model: self.spawn_model.clone(),
                variant: None,
                bootstrap: true,
            })
            .await?;
        // любой сбой после создания bootstrap гасит его (нет утечки сессий)
        let result = self
            .spawn_stage_inner(sessions, &bootstrap, &env_repo, board, task, slot)
            .await;
        let _ = sessions.delete_session(&bootstrap).await;
        result
    }

    async fn spawn_stage_inner(
        &self,
        sessions: &Gateway,
        bootstrap: &str,
        env_repo: &str,
        board: &BoardPlan,
        task: &Task,
        slot: Slot,
    ) -> UsecaseResult<String> {
        sessions
            .run_command(bootstrap, "task-env", &task.number().to_string())
            .await?;
        // Фабрика пишет Branch-ячейку: env готов, ветка work/task-N
        // существует (ADR-041 «ячейки доски пишет фабрика»); без неё PR
        // финализации невидим на карточке (инцидент #1152). Сбой записи =
        // сбой спавна: bootstrap гасится, ретрай следующим тиком (без
        // LLM-бюджета — bootstrap мимо кассы, рабочая сессия не создана).
        let branch = uwuwu_domain::BranchName::parse(&format!("work/task-{}", task.number()))
            .ok_or_else(|| {
                UsecaseError::Board(format!(
                    "ветка work/task-{} не проходит BranchName::parse",
                    task.number()
                ))
            })?;
        self.board
            .set_branch(&board.board, task.number(), &branch)
            .await?;
        let work = sessions
            .create_session(&SessionSpec {
                directory: env_repo.to_string(),
                title: format!("#{} {}", task.number(), task.title()),
                custom_agent: None,
                agent: AgentSlot::of(Agent::of_slot(slot)),
                model: self.spawn_model.clone(),
                variant: None,
                bootstrap: false,
            })
            .await?;
        let prompt = spawn_prompt(board, task, slot);
        sessions.prompt(&work, &prompt).await?;
        tracing::info!(event = "spawn.done", number = task.number(), slot = ?slot,
            session = %work, env = %env_repo, "Spawn done");
        Ok(work)
    }

    /// Wakeup: мёртвая сессия — респавн (dead-детект ПЕРВЫМ: даже без свежих
    /// комментов — строка не висит, DS «dead-детект решает: будить или
    /// респавнить»); живая — будим по свежим комментам (новее активности).
    /// Политика запуска едина для всех путей (UC-05, #1200): пик-окно
    /// откладывает не-high и будильник, и респавн; CUP-кап держит респавн
    /// (новая сессия — новый LLM-слот; будильник будит уже учтённую строку).
    pub async fn wakeup_if_fresh(
        &self,
        board: &BoardPlan,
        policy: SpawnPolicy,
        task: &Task,
        global_active: usize,
    ) -> UsecaseResult<WakeAct> {
        let now = self.clock.utc_minute();
        let Some(slot) = task.stage_slot() else {
            return Ok(WakeAct::None);
        };
        let Some(session) = task.session_of(slot) else {
            return Ok(WakeAct::None);
        };
        // dead-детект до чтения комментов: дедуп/пустой журнал не должны
        // прятать мёртвую сессию стадии (находка мок-стенда).
        // Unknown (гейт недоступен — рестарт serve, сеть) ≠ мёртвая:
        // спешный respawn плодил бы дубли на каждом окне недоступности
        // (15.11: рестарт opencode serve) — пропускаем тик, повторим.
        match self
            .sessions
            .project(&board.project)
            .liveness(session)
            .await
        {
            Liveness::Alive => {
                // будильник: пик держит не-high (живой агент в окне не будим)
                if policy.peak.holds(now) && task.priority() != Priority::High {
                    tracing::info!(
                        event = "wakeup.deferred_peak",
                        number = task.number(),
                        "Wakeup deferred: peak window"
                    );
                    return Ok(WakeAct::Deferred);
                }
            }
            Liveness::Unknown => {
                tracing::warn!(event = "wakeup.liveness_unknown", number = task.number(),
                    session = %session,
                    "Liveness unknown (гейт недоступен?) — без respawn, повтор на тике");
                return Ok(WakeAct::None);
            }
            Liveness::Dead => {
                // респавн = фактический спавн — полный гейт (пик + CUP)
                let spawn = Action::Spawn { slot };
                if gate(task, &spawn, policy, global_active, now) != Gate::Allowed {
                    tracing::info!(
                        event = "respawn.held_by_policy",
                        number = task.number(),
                        "Respawn held: пик-окно/CUP"
                    );
                    return Ok(WakeAct::Deferred);
                }
                let spawned = self.respawn_dead_stage(board, task, slot, session).await?;
                // луп-гвард эскалировал (Needs attention, без спавна) —
                // счётчик запусков не трогаем, сталл-пасс мягко откажется
                return Ok(if spawned {
                    WakeAct::Respawned
                } else {
                    WakeAct::None
                });
            }
        }
        let comments = self.board.latest_comments(task.number()).await?;
        // Хвост непрочитанных: всё после последнего доставленного (seen).
        // За один непрерывный ход агента копятся несколько комментов
        // (вердикт ревьювера + ответ владельца) — доставка только
        // последнего поглощала промежуточные навсегда (#1206 Common-1).
        let tail: Vec<CommentRef> = match self
            .state
            .last_comment_id(&board.board, task.number())
            .as_deref()
        {
            Some(seen_id) => match comments.iter().position(|c| c.id == seen_id) {
                Some(i) => comments.into_iter().skip(i + 1).collect(),
                // seen вымылся из журнала — доставляем последний
                None => comments.into_iter().next_back().into_iter().collect(),
            },
            // seen ещё нет — доставляем последний (журнал целиком не
            // спамим: у старых задач он длинный)
            None => comments.into_iter().next_back().into_iter().collect(),
        };
        let Some(last) = tail.last().cloned() else {
            return Ok(WakeAct::None);
        };
        {
            let sessions = self.sessions.project(&board.project);
            let activity = sessions.last_activity_ms(session).await?;
            let fresh = last.created_ms > activity.unwrap_or(0);
            // Ход завершён (последнее сообщение сессии — финальный ответ
            // агента): коммиты хвоста созданы во время хода — активность
            // всегда новее, fresh=false навсегда; доставляем сразу после
            // ответа агента (High-1 #1206: недостижимая «активность
            // застынет ниже коммента» заменена честным idle-критерием)
            let idle = !fresh && self.session_idle(board, session).await;
            if fresh || idle {
                let body = tail
                    .iter()
                    .map(|c| c.body.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n---\n\n");
                sessions
                    .prompt(
                        session,
                        &format!(
                            "Wakeup: новые комменты в задаче #{}:\n\n{}",
                            task.number(),
                            body
                        ),
                    )
                    .await?;
                tracing::info!(
                    event = "wakeup.delivered",
                    number = task.number(),
                    tail = tail.len(),
                    idle,
                    "Wakeup delivered"
                );
                self.state
                    .set_last_comment_id(&board.board, task.number(), &last.id);
                return Ok(WakeAct::Woke);
            }
            // Агент ещё работает (последнее сообщение — промпт/tool-call):
            // seen НЕ пишем, доставка повторится на тике после ответа
            Ok(WakeAct::None)
        }
    }

    /// Ход сессии завершён: последнее сообщение — финальный ассистентский
    /// ответ (после него нет ни промптов, ни tool-call'ов). Err трактуем
    /// консервативно («не завершён») — пинги и доставки не спешат.
    /// Гейтвей — исполнителя проекта (#1203).
    pub(crate) async fn session_idle(&self, board: &BoardPlan, session: &str) -> bool {
        matches!(
            self.sessions
                .project(&board.project)
                .last_assistant_reply(session)
                .await,
            Ok(Some(reply)) if reply.is_latest && reply.finished
        )
    }

    /// Респавн мёртвой сессии стадии через луп-гвард (attention-механика:
    /// респавн = повтор слота при неизменном статусе; >2 → Needs attention).
    /// Политика запуска (пик/CUP) проверена вызывающим wakeup-путём.
    /// Возвращает true, если спавн состоялся (эскалация — false).
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
        Ok(true)
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

pub(crate) fn spawn_prompt(board: &BoardPlan, task: &Task, slot: Slot) -> String {
    let stage = match slot {
        Slot::Design => "стадия утряски: изучи постановку, уточни у владельца, собери план",
        Slot::Dev => "реализация по утверждённому плану; веди задачу по канону AGENTS.md",
        Slot::PlanReview => "ревью плана: парафриз/zoom-out/readiness, вердикт комментом",
        Slot::ResultReview => "ревью результата: код/CI/отчёт, вердикт комментом",
        Slot::Test => "тестирование: качество фичи, вердикт pass/fail с шагами",
    };
    let finish = match slot {
        Slot::Design => "план в тело → переведи статус в «Ревью плана»",
        Slot::Dev => "код/CI/PR/отчёт готовы → переведи статус в «Ревью результата»",
        Slot::PlanReview => "вердикт комментом → approve/request_changes и статус по вердикту",
        Slot::ResultReview => "вердикт комментом → approve/request_changes и статус по вердикту",
        Slot::Test => "вердикт pass/fail комментом → статус Review или In Progress",
    };
    format!(
        "Задача #{number} «{title}» (доска «{board}», проект {project}).\n\
         Ты — агент конвейера uwuwu (методичка репо). {stage}.\n\
         \n\
         ⚠ Контракт хода: твоя сессия ЗАСЫПАЕТ после ответа — конвейер не\n\
         продолжит работу, пока ты не закроешь транзакцию. Ход не завершай\n\
         без одного из двух:\n\
         1) статусный перевод (uwuwu_task_edit {{number, status}}): {finish};\n\
         2) вопрос владельцу комментом (uwuwu_task_comment) + статус\n\
         «Needs attention».\n\
         «Поработал и замолчал» = строка зависает до автопинга конвейера\n\
         (15 мин при завершённом ходе, до 60 мин — если ход ещё идёт).\n\
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
