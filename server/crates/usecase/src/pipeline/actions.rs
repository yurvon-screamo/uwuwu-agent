//! Действия тика: спавн стадии (bootstrap /command → сессия в env),
//! wakeup по свежим комментам, автомерж (проба PR → мерж без закрытия / bounce).

use crate::error::{UsecaseError, UsecaseResult};
use crate::naming;
use crate::pipeline::tick::{
    BoardPlan, RowContext, SpawnHold, TickPlan, TickUseCase, SESSION_LOST_ESCALATE_AFTER,
};
use crate::ports::{CommentRef, Liveness, SessionSpec};
use crate::router::Gateway;
use uwuwu_domain::{
    gate, sighting_known, Action, Agent, AgentSlot, Gate, LoopOutcome, Priority, Slot, SpawnLoop,
    SpawnPolicy, Status, Task,
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
    /// Гард «живая сессия в env» держит запуск (#1163).
    Guarded,
    /// Список исполнителя недоступен — респавн без зрения невозможен (#1263).
    GuardedBlind,
    /// Нечего будить: тишина, коммент уже отработан или луп-эскалация.
    None,
}

/// Итог маршрутизации потерянной сессии (#1288).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LostAct {
    /// Resurrection удалась — новая сессия в ячейке.
    Respawned,
    /// Порог потерянных доставок достигнут — Needs attention.
    Escalated,
    /// Гвард держит respawn: живая сестра в env строки.
    Guarded,
    /// Список исполнителя недоступен — hold без зрения (#1263).
    GuardedBlind,
    /// Пик/CUP держит respawn — ретрай следующим тиком.
    Deferred,
    /// Десинк-счётчик растёт внутри порога; либо луп-гвард сам
    /// эскалировал / внутренний отказ — исход учтён без outcome-счётчика.
    Counted,
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
        let env_repo = board.env_repo_dir(task.number());
        let title = session_title(task, slot);
        let boot_model = self.models.for_default();
        let bootstrap = sessions
            .create_session(&SessionSpec {
                directory: board.checkout.clone(),
                title: format!("bootstrap {title}"),
                custom_agent: None,
                agent: AgentSlot::Default,
                model: boot_model.model,
                variant: boot_model.variant,
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
                    "branch work/task-{} fails BranchName::parse",
                    task.number()
                ))
            })?;
        self.board
            .set_branch(&board.board, task.number(), &branch)
            .await?;
        // #1300: с пином агента дев-семья берёт модель из developer-ключа
        // карты (раньше шла по default-ключу); переход пинен тестом
        // dev_spawn_pins_the_developer_agent, ключи канонически одинаковы
        let work_model = match Agent::of_slot(slot) {
            Some(agent) => self.models.for_agent(agent),
            None => self.models.for_default(),
        };
        let work = sessions
            .create_session(&SessionSpec {
                directory: env_repo.to_string(),
                title: session_title(task, slot),
                custom_agent: None,
                agent: AgentSlot::of(Agent::of_slot(slot)),
                model: work_model.model,
                variant: work_model.variant,
                bootstrap: false,
            })
            .await?;
        let prompt = spawn_prompt(board, task, slot);
        if let Err(e) = sessions.prompt(&work, &prompt).await {
            // Сирота (#1163): сессия создана без промпта — работать ей не
            // дано, гасим (симметрично bootstrap-зачистке вызывающего).
            let _ = sessions.delete_session(&work).await;
            return Err(e);
        }
        tracing::info!(event = "spawn.done", number = task.number(), slot = ?slot,
            session = %work, env = %env_repo, "Spawn done");
        Ok(work)
    }

    /// Wakeup: мёртвая сессия — респавн (dead-детект ПЕРВЫМ: даже без свежих
    /// комментов — строка не висит, DS «dead-детект решает: будить или
    /// респавнить»); живая — будим по свежим комментам (новее активности).
    /// Wakeup: мёртвая сессия — респавн (dead-детект ПЕРВЫМ: даже без свежих
    /// комментов — строка не висит, DS «dead-детект решает: будить или
    /// респавнить»); живая — будим по свежим комментам (новее активности).
    /// Политика запуска едина для всех путей (UC-05, #1200/#1231): пик-окно
    /// откладывает не-high и будильник, и респавн; CUP-кап держит и будильник,
    /// и респавн — оба запускают ход (LLM-слот): будильник доставляет промпт
    /// в idle-сессию, респавн создаёт новую сессию.
    pub(crate) async fn wakeup_if_fresh(
        &self,
        board: &BoardPlan,
        policy: SpawnPolicy,
        task: &Task,
        global_active: usize,
        ctx: &RowContext<'_>,
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
                // Second source (#1072→#1163): сессия в списке исполнителя —
                // проба солгала (serve-шторм 13.11 давал 404 живым ходам);
                // респавн отменяется, вердикт уйдёт wakeup-путём на тике,
                // когда проба вернётся к правде. Штормы false-dead закрыты
                // этим чеком — кулдаун-чек на респавн не вешается: он
                // замедлял бы attention-механику (луп-гвард) в 5 раз
                // (эскалация петли час вместо ~15 мин).
                if sighting_known(ctx.sightings.unwrap_or(&[]), session) {
                    tracing::warn!(event = "wakeup.second_source_alive", number = task.number(),
                        session = %session,
                        "Проба Dead при живой сессии в списке исполнителя — респавн отменён");
                    return Ok(WakeAct::None);
                }
                // Env-гард на респавне (ревью Common-2 + зрение #1263):
                // сеанс ячейки мёртв по-настоящему, но в env строки может
                // жить сирота из crash-окна — респавн поверх живого хода =
                // тот же класс дубля, что и на plain-пути. Слепой скан —
                // респавн без зрения невозможен, hold до восстановления.
                match self.spawn_hold(ctx, board, task, slot, self.clock.now_ms()) {
                    Some(SpawnHold::BlindScan) => {
                        tracing::info!(
                            event = "respawn.held_blind_scan",
                            number = task.number(),
                            slot = naming::slot_key(slot),
                            "Респавн держится: список исполнителя недоступен — спавн без зрения невозможен (#1263)"
                        );
                        return Ok(WakeAct::GuardedBlind);
                    }
                    Some(SpawnHold::Live) => {
                        tracing::warn!(
                            event = "respawn.held_live_session",
                            number = task.number(),
                            slot = naming::slot_key(slot),
                            "Респавн держится: живая сессия в env строки (second source #1163)"
                        );
                        return Ok(WakeAct::Guarded);
                    }
                    None => {}
                }
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
                match self.respawn_dead_stage(board, task, slot, session).await {
                    Ok(true) => return Ok(WakeAct::Respawned),
                    // луп-гвард эскалировал (Needs attention, без спавна) —
                    // счётчик запусков не трогаем, сталл-пасс мягко откажется
                    Ok(false) => return Ok(WakeAct::None),
                    // анти-ре-энтри (#1288): отказ респавна — включая
                    // SessionNotFound сирота-пути (404 промпта свежей
                    // сессии) — не возвращается в маршрутизацию потери;
                    // лог и ретрай следующим тиком
                    Err(e) => {
                        self.log_action_failed(task, e);
                        return Ok(WakeAct::None);
                    }
                }
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
            // Рождение читателя: метка последнего спавна слота. Реестр-коммент,
            // изданный до рождения сессии, никогда не был для неё «новым» —
            // спавн-промпт несёт контекст (#1204). Нет метки (внешний claim,
            // legacy-state) — член рождения выключен, семантика прежняя.
            let spawned_at = self.state.last_spawn_ms(&board.board, task.number(), slot);
            let fresh = last.created_ms > activity.unwrap_or(0);
            // Ход завершён (последнее сообщение сессии — финальный ответ
            // агента): коммиты хвоста созданы во время хода — активность
            // всегда новее, fresh=false навсегда; доставляем сразу после
            // ответа агента (High-1 #1206: недостижимая «активность
            // застынет ниже коммента» заменена честным idle-критерием)
            let idle = !fresh && self.session_idle(board, session).await;
            if fresh || idle {
                let foreign = self.slice_self_echo(task.number(), &tail, activity, spawned_at);
                if foreign.is_empty() {
                    // Хвост целиком свой (#1204): эхо-будки нет. Поглощение
                    // (тихий сдвиг бейзлайна) допустимо, только когда читатель
                    // родился после хвоста — такой хвост не достанется этому
                    // слоту ни в одном будущем раунде. Читатель старше хвоста
                    // (автор: вердикт опубликован в его же ход) — коммент
                    // адресован СЛЕДУЮЩЕМУ держателю: seen не двигается,
                    // иначе тик в окне «вердикт опубликован → статус
                    // переведён» съедает доставку держателю (#1533).
                    if spawned_at.is_some_and(|ms| ms > last.created_ms) {
                        self.state
                            .set_last_comment_id(&board.board, task.number(), &last.id);
                    }
                    return Ok(WakeAct::None);
                }
                // CUP-гвард (#1231): доставка промпта — новый ход, тот же
                // LLM-слот, что и спавн. «Будильник будит уже учтённую
                // строку» было неверно: idle-сессия в подсчёт капа не
                // входит. При полном капе — отложить, seen не писать:
                // доставка повторится на свободном тике, комменты не
                // теряются (см. WakeAct::Deferred).
                if global_active >= policy.cup {
                    tracing::info!(
                        event = "wakeup.held_cup",
                        number = task.number(),
                        active = global_active,
                        cap = policy.cup,
                        "Wakeup held: CUP cap"
                    );
                    return Ok(WakeAct::Deferred);
                }
                let body = foreign
                    .iter()
                    .map(|c| c.body.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n---\n\n");
                // Пин агента слота перед ходом (#1300): именованным слотам —
                // no-op (сессия уже носит агента), легаси-дев-сессии —
                // фикс класса «wake перебросил кости» резолва исполнителя.
                // Отказ — Err без seen: доставка повторится следующим тиком.
                if let Some(agent) = Agent::of_slot(slot) {
                    sessions.switch_agent(session, agent).await?;
                }
                sessions
                    .prompt(
                        session,
                        &format!(
                            "Wakeup: new comments on task #{}:\n\n{}",
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

    /// Own-срез self-эха (#1204): коммент хвоста — «свой» для держащей
    /// сессии, если он из реестра серверных постов (MCP task_comment) и не
    /// свежее её последней активности — ход после поста двигает активность
    /// (инвариант own-среза ADR-050) — либо не свежее её рождения: коммент,
    /// изданный до спавна сессии, спавн-промптом уже покрыт (#1533, член
    /// рождения). Чужие агентские (вердикты других стадий) и юзерские будят.
    /// Ни активность, ни рождение недоступны — без среза: лишняя
    /// эхо-будка лучше потерянного коммента (fail-open, семантика TS).
    fn slice_self_echo<'a>(
        &self,
        number: u64,
        tail: &'a [CommentRef],
        activity: Option<u64>,
        spawned_at: Option<u64>,
    ) -> Vec<&'a CommentRef> {
        if activity.is_none() && spawned_at.is_none() {
            return tail.iter().collect();
        }
        let foreign: Vec<&CommentRef> = tail
            .iter()
            .filter(|c| {
                !(self.state.own_comment(&c.id)
                    && activity
                        .into_iter()
                        .chain(spawned_at)
                        .any(|at| c.created_ms <= at))
            })
            .collect();
        let own = tail.len() - foreign.len();
        if own > 0 {
            tracing::info!(
                event = "wakeup.self_echo_sliced",
                number,
                own,
                foreign = foreign.len(),
                "Self-echo sliced: own comments do not wake the holder"
            );
        }
        foreign
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
                        "⚠︎ Respawn loop: the `{}` stage session dies and respawns \
                         >2 times without status progress ({:?}). Owner's triage.",
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
        // кулдаун едина с plain-путём (#1163): заряд до записи ячейки —
        // отказ set_session не открывает респавн-шторм (ночь 15.11)
        self.state
            .mark_spawn(&board.board, task.number(), slot, self.clock.now_ms());
        if let Err(e) = self
            .board
            .set_session(&board.board, task.number(), slot, &fresh)
            .await
        {
            // сирота (#1163): сеанс без ячейки гасим — будить его некому
            let _ = self
                .sessions
                .project(&board.project)
                .delete_session(&fresh)
                .await;
            return Err(e);
        }
        Ok(true)
    }

    /// Маршрутизация потерянной сессии стадии (#1288): классифицированный
    /// SessionNotFound доставки приходит из обеих Err-веток
    /// observe_stage_session. Руки: сессия в списке исполнителя (десинк) —
    /// счётчик и парковка, респавн запрещён анти-дублем (список — источник
    /// истины гарда спавна, #1163/#1263); сессии нет в списке —
    /// resurrection гвардами Dead-ветки (env-гард → пик/CUP →
    /// respawn_dead_stage с луп-гвардом). Отказы респавна обрабатываются
    /// внутри (анти-ре-энтри: повторного входа в маршрутизацию в том же
    /// тике нет).
    pub(crate) async fn session_lost_pass(
        &self,
        board: &BoardPlan,
        plan: &TickPlan,
        task: &Task,
        global_active: usize,
        ctx: &RowContext<'_>,
    ) -> UsecaseResult<LostAct> {
        // обе Err-ветки — stage-пути со слотом и сессией в ячейке;
        // раскладка вне контракта — no-op исход (исход учтён логом Err)
        let Some(slot) = task.stage_slot() else {
            return Ok(LostAct::Counted);
        };
        let Some(session) = task.session_of(slot) else {
            return Ok(LostAct::Counted);
        };
        let Some(sightings) = ctx.sightings else {
            tracing::info!(
                event = "lost.held_blind_scan",
                number = task.number(),
                slot = naming::slot_key(slot),
                "Сессия потеряна, список исполнителя недоступен — держим без зрения (#1263)"
            );
            return Ok(LostAct::GuardedBlind);
        };
        if sighting_known(sightings, session) {
            return self.session_lost_desync(board, task, slot).await;
        }
        // resurrection-рука: исполнитель сам подтвердил отсутствие сессии
        if matches!(
            self.spawn_hold(ctx, board, task, slot, self.clock.now_ms()),
            Some(SpawnHold::Live)
        ) {
            tracing::warn!(
                event = "lost.held_live_session",
                number = task.number(),
                slot = naming::slot_key(slot),
                "Респавн потерянной сессии держится: живая сестра в env строки"
            );
            return Ok(LostAct::Guarded);
        }
        let spawn = Action::Spawn { slot };
        if gate(
            task,
            &spawn,
            plan.policy,
            global_active,
            self.clock.utc_minute(),
        ) != Gate::Allowed
        {
            tracing::info!(
                event = "lost.held_by_policy",
                number = task.number(),
                "Respawn потерянной сессии held: пик/CUP"
            );
            return Ok(LostAct::Deferred);
        }
        match self.respawn_dead_stage(board, task, slot, session).await {
            Ok(true) => {
                // resurrection удалась — серия потерянных доставок прервана
                self.state.clear_lost_session(&board.board, task.number());
                Ok(LostAct::Respawned)
            }
            // луп-гвард эскалировал сам (Needs attention) — исход учтён им
            Ok(false) => Ok(LostAct::Counted),
            // анти-ре-энтри: отказ респавна не возвращается в маршрутизацию
            Err(e) => {
                self.log_action_failed(task, e);
                Ok(LostAct::Counted)
            }
        }
    }

    /// Десинк-рука: исполнитель числит сессию живой, но доставку
    /// отклоняет (класс serve-рассинхрона/шторма 13.11). Респавн
    /// запрещён анти-дублем; счётчик потерянных доставок
    /// (статус-тегированный) до порога → Needs attention с причиной.
    async fn session_lost_desync(
        &self,
        board: &BoardPlan,
        task: &Task,
        slot: Slot,
    ) -> UsecaseResult<LostAct> {
        let (prev_status, count) = self
            .state
            .lost_session(&board.board, task.number())
            .map(|(st, c)| (Some(st), c))
            .unwrap_or((None, 0));
        let count = if prev_status.as_ref() == Some(&task.status()) {
            count
        } else {
            0
        };
        let next = count + 1;
        self.state
            .mark_lost_session(&board.board, task.number(), task.status());
        if next < SESSION_LOST_ESCALATE_AFTER {
            tracing::warn!(
                event = "lost.desync_counted",
                number = task.number(),
                slot = naming::slot_key(slot),
                count = next,
                "Доставка 404 при живом списке исполнителя — счётчик потери"
            );
            return Ok(LostAct::Counted);
        }
        tracing::warn!(
            event = "lost.escalated",
            number = task.number(),
            slot = naming::slot_key(slot),
            "Сессия потеряна: доставка 404 при живом списке — порог неудач"
        );
        self.board
            .set_status(&board.board, task.number(), Status::NeedsAttention)
            .await?;
        self.board
            .post_comment(
                task.number(),
                &format!(
                    "⚠︎ Session lost: delivery to the `{}` stage session is rejected \
                     by the executor (404) while the session list is alive — the \
                     {SESSION_LOST_ESCALATE_AFTER} failure threshold is hit. The row is in \
                     Needs attention, owner's triage.",
                    naming::slot_key(slot)
                ),
            )
            .await?;
        self.state.clear_lost_session(&board.board, task.number());
        Ok(LostAct::Escalated)
    }

    /// Автомерж (#1232): проба PR → мерж (строка остаётся в Review — Done
    /// всегда за владельцем) / bounce в работу. Ветка без PR — возврат в
    /// работу (PR ещё не создан); docs-only до сюда не доходит (домен
    /// automerge_ready не прощупывает безветочные ячейки — Vcs::Local и
    /// Artifact, #1155 — и PR-ячейки с URL не PR-формы: wiki/issues/blob
    /// с фабричной веткой, #1286 — мержить нечего, строка ждёт владельца).
    /// Возвращает true — PR смержен этим вызовом (счётчик тика).
    pub async fn automerge(&self, board: &BoardPlan, task: &Task) -> UsecaseResult<bool> {
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
                event = "automerge.pending",
                number = task.number(),
                "Automerge: PR pending (mergeable/чекси считаются) — ждём следующего тика"
            );
            return Ok(false);
        }
        match state {
            PrState::Pending => unreachable!("pending обработан выше"),
            // Уже влит (вручную или раньше): no-op — merge-API не зовётся,
            // строка остаётся в Review и ждёт владельца (инцидент #1151/#1162:
            // ложный bounce вручную смерженной строки недопустим)
            PrState::Merged => {
                tracing::info!(
                    event = "automerge.merged",
                    number = task.number(),
                    "Automerge: PR уже смержен — строка ждёт владельца в Review"
                );
                Ok(false)
            }
            PrState::Foreign => {
                tracing::info!(
                    event = "automerge.foreign",
                    number = task.number(),
                    "Automerge: PR-URL неуправляемого вендора — строка ждёт владельца в Review (#1269)"
                );
                Ok(false)
            }
            PrState::Missing => {
                self.bounce(board, task, "branch set, PR not found").await?;
                Ok(false)
            }
            PrState::Closed => {
                self.bounce(board, task, "PR closed without a merge")
                    .await?;
                Ok(false)
            }
            PrState::Open {
                mergeable: false, ..
            } => {
                self.bounce(board, task, "PR conflicts with master").await?;
                Ok(false)
            }
            PrState::Open {
                ci_green: false, ..
            } => {
                self.bounce(board, task, "PR has red CI").await?;
                Ok(false)
            }
            PrState::Open {
                mergeable: true,
                ci_green: true,
            } => {
                let pr = task.pr().map(|u| u.as_str()).unwrap_or_default();
                match self.prs.merge(&board.repo, pr).await {
                    Ok(()) => {
                        // мерж без закрытия: строка остаётся в Review —
                        // финализация (Done) всегда за владельцем (#1232)
                        tracing::info!(
                            event = "automerge.merged",
                            number = task.number(),
                            "Automerge: PR смержен — строка остаётся в Review"
                        );
                        Ok(true)
                    }
                    Err(e) => {
                        self.bounce(board, task, &format!("merge failed: {e}"))
                            .await?;
                        Ok(false)
                    }
                }
            }
        }
    }

    async fn bounce(&self, board: &BoardPlan, task: &Task, why: &str) -> UsecaseResult<()> {
        self.board
            .set_status(&board.board, task.number(), Status::InProgress)
            .await?;
        self.board
            .post_comment(task.number(), &format!("Automerge: back to work — {why}."))
            .await?;
        tracing::info!(
            event = "automerge.bounce",
            number = task.number(),
            why,
            "Automerge: bounce"
        );
        Ok(())
    }
}

/// Тайтл конвейерной сессии: `#<номер> <эмодзи стадии> <тайтл>` (задача
/// #1212); единая точка сборки — bootstrap наследует тем же значением.
fn session_title(task: &Task, slot: Slot) -> String {
    format!("#{} {} {}", task.number(), slot.emoji(), task.title())
}

/// Дословные строки стадий в spawn_prompt — контракт мок-стенда:
/// `mock_opencode::stage_action_of_prompt` классифицирует спавн по этим
/// фрагментам (импортирует константу — правка стадии через этот массив,
/// расхождение источника и мока ловится тестом
/// `stage_prompts_spawned_per_slot`).
pub const STAGE_PROMPTS: &[(Slot, &str)] = &[
    (
        Slot::Design,
        "design stage: study the statement, clarify with the owner, build the plan",
    ),
    (
        Slot::Dev,
        "implementation per the approved plan; run the task per the AGENTS.md canon",
    ),
    (
        Slot::PlanReview,
        "plan review: paraphrase/zoom-out/readiness, verdict in a comment",
    ),
    (
        Slot::ResultReview,
        "result review: code/CI/report, verdict in a comment",
    ),
    (
        Slot::Test,
        "testing: feature quality, pass/fail verdict with steps",
    ),
    (
        Slot::TestReview,
        "testing review: the branch's test delta, verdict in a comment",
    ),
];

pub(crate) fn spawn_prompt(board: &BoardPlan, task: &Task, slot: Slot) -> String {
    let stage = STAGE_PROMPTS
        .iter()
        .find(|(s, _)| *s == slot)
        .map(|(_, m)| *m)
        .unwrap_or_default();
    let finish = match slot {
        Slot::Design => "plan into the body → set the status to «Plan review»",
        Slot::Dev => "code/CI/PR/report ready → set the status to «Result review»",
        Slot::PlanReview => {
            "verdict in a comment → approve/request_changes and the status per the verdict"
        }
        Slot::ResultReview => {
            "verdict in a comment → approve/request_changes and the status per the verdict"
        }
        Slot::Test => {
            "pass/fail verdict in a comment → pass: with (tester) commits — «Testing review»; without — «Review»; fail — In Progress"
        }
        Slot::TestReview => {
            "verdict in a comment → «Review» (clean) or «Testing» (findings) per the verdict"
        }
    };
    format!(
        "Task #{number} «{title}» (board «{board}», project {project}).\n\
         You are a uwuwu pipeline agent (repo canon). {stage}.\n\
         \n\
         ⚠ Turn contract: your session SLEEPS after the reply — the pipeline will not\n\
         continue until you close the transaction. Do not end the turn without one of:\n\
         1) a status transition (uwuwu_task_edit {{number, status}}): {finish};\n\
         2) a question to the owner in a comment (uwuwu_task_comment) + status\n\
         «Needs attention».\n\
         «Worked and went silent» = the row hangs until the pipeline auto-ping\n\
         (15 min after a finished turn, up to 60 min while the turn is in progress).\n\
         \n\
         The platform goes through the uwuwu server MCP tools (Code Mode: tools.uwuwu.*):\n\
         - task body and comments: uwuwu_task_get {{number: {number}}}\n\
         - owner persona (read first): uwuwu_person_show\n\
         - project canon docs: uwuwu_docs_read {{slug: \"Projects/{project}/README\"}},\n\
           Design Spec (Projects/{project}/Design Spec; if absent — a normal skip), Deployment\n\
         - run the task: uwuwu_task_edit / task_comment / task_claim {{session: your ses_…}}\n\
         \n\
         Work per the AGENTS.md canon; reports and questions go in comments (uwuwu_task_comment).",
        number = task.number(),
        title = task.title(),
        board = board.board,
        project = board.project,
    )
}
