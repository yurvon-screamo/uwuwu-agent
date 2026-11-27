//! Действия тика: спавн стадии (bootstrap /command → сессия в env),
//! wakeup по свежим комментам, автомерж (проба PR → мерж без закрытия / bounce).

use crate::error::{UsecaseError, UsecaseResult};
use crate::naming;
use crate::pipeline::tick::{
    BoardPlan, CupGate, RowContext, SpawnHold, TickOutcome, TickPlan, TickUseCase,
    SESSION_LOST_ESCALATE_AFTER,
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
            .run_command(
                bootstrap,
                "task-env",
                &task.number().to_string(),
                TASK_ENV_COMMAND_BUDGET_MS,
            )
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
        // Свежая сессия получила собственный промпт — pending раунд-промпт
        // мастер-сессии погашен (#1626: мёртвая ячейка снимается переходом,
        // пересоздание несёт промпт само; маркер без этого гашения дал бы
        // двойную доставку в новую сессию).
        self.state
            .clear_round_reentry_pending(&board.board, task.number());
        tracing::info!(event = "spawn.done", number = task.number(), slot = ?slot,
            session = %work, env = %env_repo, "Spawn done");
        Ok(work)
    }

    /// Раунд-промпт мастер-сессии ревьювера (#1626 срез 2): re-entry в
    /// ревью-стадию при живой сессии доставляется в неё же — нативный
    /// компакт (детерминированный триггер: каждый re-entry, владелец
    /// 25.11: «автокомпактом этой сессии перед тем как делать к ней новый
    /// цикл») и спавн-промпт стадии. Гейты те же, что у будильника: пик
    /// проверен вызывающим, CUP — здесь; ход сессии в полёте — доставка
    /// на тике после ответа (маркер persists). Some — раунд-промпт
    /// решён (Woke/Deferred), None — pending нет, дальше по будильнику.
    async fn deliver_round_prompt(
        &self,
        board: &BoardPlan,
        task: &Task,
        slot: Slot,
        session: &str,
        cup: CupGate,
    ) -> UsecaseResult<Option<WakeAct>> {
        if !slot.is_review()
            || !self
                .state
                .round_reentry_pending(&board.board, task.number())
        {
            return Ok(None);
        }
        if cup.full() {
            tracing::info!(
                event = "wakeup.round_prompt_held_cup",
                number = task.number(),
                active = cup.active_now,
                cap = cup.policy.cup,
                tier = "continuation",
                "Раунд-промпт отложен: CUP cap"
            );
            return Ok(Some(WakeAct::Deferred));
        }
        if !self.session_idle(board, session).await {
            return Ok(Some(WakeAct::None));
        }
        let sessions = self.sessions.project(&board.project);
        sessions.compact_session(session).await?;
        // Пин агента слота перед ходом (#1300) — как на будильнике
        if let Some(agent) = Agent::of_slot(slot) {
            sessions.switch_agent(session, agent).await?;
        }
        sessions
            .prompt(session, &spawn_prompt(board, task, slot))
            .await?;
        self.state
            .clear_round_reentry_pending(&board.board, task.number());
        tracing::info!(
            event = "wakeup.round_prompt_delivered",
            number = task.number(),
            slot = naming::slot_key(slot),
            session = %session,
            "Раунд-промпт доставлен в мастер-сессию (компакт admitted перед новым циклом)"
        );
        Ok(Some(WakeAct::Woke))
    }

    /// Re-entry wake тестера (#1607): повторный вход в Testing при живой
    /// мастер-сессии и пустом чужом хвосте — будить нечем, вход доставляет
    /// детерминированный дельта-промпт. Без компакта: контекст прошлых
    /// раундов — ценность мастер-сессии, линию держит нативный порог
    /// (срез 3). Гейты — как у будильника: CUP держит (маркер persists),
    /// ход в полёте — доставка на тике после ответа. Err — маркер открыт,
    /// ретрай следующим тиком.
    async fn deliver_tester_reentry(
        &self,
        board: &BoardPlan,
        task: &Task,
        session: &str,
        global_active: usize,
        policy: SpawnPolicy,
    ) -> UsecaseResult<WakeAct> {
        if global_active >= policy.cup {
            tracing::info!(
                event = "wakeup.tester_reentry_held_cup",
                number = task.number(),
                active = global_active,
                cap = policy.cup,
                tier = "continuation",
                "Re-entry wake тестера отложен: CUP cap"
            );
            return Ok(WakeAct::Deferred);
        }
        if !self.session_idle(board, session).await {
            return Ok(WakeAct::None);
        }
        let sessions = self.sessions.project(&board.project);
        // Пин агента слота перед ходом (#1300) — как на будильнике.
        // Отказ — Err без гашения маркера: доставка повторится тиком.
        if let Some(agent) = Agent::of_slot(Slot::Test) {
            sessions.switch_agent(session, agent).await?;
        }
        sessions
            .prompt(
                session,
                &format!(
                    "Repeat testing round for task #{}: run the delta per Delta mode (agents/tester.md) — verdict only after a real delta run. \
                     If the owner answered a Needs attention question, fix the reply verbatim in the body/thread and pass owner_answer when clearing it.",
                    task.number()
                ),
            )
            .await?;
        self.state
            .clear_round_reentry_pending(&board.board, task.number());
        tracing::info!(
            event = "wakeup.tester_reentry_delivered",
            number = task.number(),
            session = %session,
            "Re-entry wake тестера доставлен в мастер-сессию"
        );
        Ok(WakeAct::Woke)
    }

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
                // будильник: пик держит не-high только у целевого провайдера
                // (#1680); живой агент другого провайдера будится в окне.
                let slot_model = Agent::of_slot(slot)
                    .map(|agent| self.models.for_agent(agent))
                    .unwrap_or_else(|| self.models.for_default());
                if crate::spawn_models::peak_applies(&slot_model)
                    && policy.peak.holds(now)
                    && task.priority() != Priority::High
                {
                    tracing::info!(
                        event = "wakeup.deferred_peak",
                        number = task.number(),
                        "Wakeup deferred: peak window"
                    );
                    return Ok(WakeAct::Deferred);
                }
                // Раунд-промпт мастер-сессии (#1626 срез 2) — та же политика
                // запуска, что у будильника
                if let Some(act) = self
                    .deliver_round_prompt(
                        board,
                        task,
                        slot,
                        session,
                        CupGate {
                            policy,
                            active_now: global_active,
                        },
                    )
                    .await?
                {
                    return Ok(act);
                }
            }
            Liveness::Unknown => {
                tracing::warn!(event = "wakeup.liveness_unknown", number = task.number(),
                    session = %session,
                    "Liveness unknown (гейт недоступен?) — без respawn, повтор на тике");
                return Ok(WakeAct::None);
            }
            Liveness::Dead => {
                // #1616 (срез 4): контракт #1357 безусловен — direct-строке
                // фабрика никого не спавнит, включая респавн по мёртвой
                // сессии; строка тихо ждёт владельца (оживление/чистка
                // ячейки — его ходы), исход None — без эскалаций
                if task.direct() {
                    tracing::debug!(
                        event = "respawn.held_direct",
                        number = task.number(),
                        slot = naming::slot_key(slot),
                        "Респавн direct-строки запрещён контрактом #1357 — строка ждёт владельца"
                    );
                    return Ok(WakeAct::None);
                }
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
            // Журнал пуст при открытом маркере тестера (#1607): будить
            // нечем — вход доставляет re-entry wake; иначе тихий пропуск.
            if slot == Slot::Test
                && self
                    .state
                    .round_reentry_pending(&board.board, task.number())
            {
                return self
                    .deliver_tester_reentry(board, task, session, global_active, policy)
                    .await;
            }
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
                    // Re-entry тестера без чужих комментов (#1607): хвост
                    // целиком свой — будить нечем, вход доставляет re-entry
                    // wake. Маркер гасится только доставкой (Woke);
                    // Deferred/Err и занятая сессия — маркер открыт, ретрай.
                    if slot == Slot::Test
                        && self
                            .state
                            .round_reentry_pending(&board.board, task.number())
                    {
                        return self
                            .deliver_tester_reentry(board, task, session, global_active, policy)
                            .await;
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
                        // будильник достижим только при сессии на слоте
                        // стадии — тир продолжений всегда
                        tier = "continuation",
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
                // Вход с чужим хвостом — доставкой владеет wakeup (#1607):
                // re-entry wake тестера на этот раунд больше не нужен.
                if slot == Slot::Test {
                    self.state
                        .clear_round_reentry_pending(&board.board, task.number());
                }
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
        // #1616 (срез 4): resurrection-рука — тоже спавн; direct-строке
        // запрещён контрактом #1357 безусловно. Исход Counted: счётчик
        // потерянных доставок ведётся, респавна и эскалаций нет — строка
        // ждёт владельца
        if task.direct() {
            tracing::debug!(
                event = "lost.held_direct",
                number = task.number(),
                slot = naming::slot_key(slot),
                "Респавн потерянной сессии direct-строки запрещён контрактом #1357"
            );
            return Ok(LostAct::Counted);
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
    /// Возвращает true — PR смержен этим вызовом (счётчик тика);
    /// семантические bounce инкрементят out.automerge_bounced (#1519).
    pub async fn automerge(
        &self,
        board: &BoardPlan,
        task: &Task,
        out: &mut TickOutcome,
    ) -> UsecaseResult<bool> {
        use crate::ports::PrState;
        let probe = self
            .prs
            .probe(
                &board.repo,
                task.pr().map(|u| u.as_str()),
                task.branch().map(|b| b.as_str()),
            )
            .await?;
        if probe.state == PrState::Pending {
            tracing::info!(
                event = "automerge.pending",
                number = task.number(),
                "Automerge: PR pending (mergeable/чекси считаются) — ждём следующего тика"
            );
            return Ok(false);
        }
        match probe.state {
            PrState::Pending => unreachable!("pending обработан выше"),
            // Уже влит (вручную или раньше, либо ячейка пуста/протухла, а
            // веточная проба нашла смерженный PR — state=all, ревью Common
            // #1519): no-op — merge-API не зовётся, строка остаётся в Review
            // и ждёт владельца (инцидент #1151/#1162: ложный bounce
            // вручную смерженной строки недопустим). Резолв известен —
            // canonical ячейка дозаполняется, Done-кандидат не остаётся
            // без ссылки на артефакт; отказ записи — warn без bounce
            // (строка и так ждёт владельца, мержа не будет).
            PrState::Merged => {
                if let Some(url) = probe.url.as_deref() {
                    if let Err(e) = self.fill_cell_if_differs(board, task, url).await {
                        tracing::warn!(
                            event = "automerge.pr_cell_fill_failed",
                            number = task.number(),
                            error = %e,
                            "Automerge: дозаполнение смерженного PR не удалось — строка ждёт владельца в Review"
                        );
                    }
                }
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
                self.bounce(board, task, "branch set, PR not found", out)
                    .await?;
                Ok(false)
            }
            PrState::Closed => {
                self.bounce(board, task, "PR closed without a merge", out)
                    .await?;
                Ok(false)
            }
            PrState::Open {
                mergeable: false, ..
            } => {
                self.bounce(board, task, "PR conflicts with master", out)
                    .await?;
                Ok(false)
            }
            PrState::Open {
                ci_green: false, ..
            } => {
                self.bounce(board, task, "PR has red CI", out).await?;
                Ok(false)
            }
            PrState::Open {
                mergeable: true,
                ci_green: true,
            } => {
                // Дозаполнение canonical PR-ячейки (#1519): вердикт о
                // конкретном PR есть, ячейка пуста или несёт протухший URL —
                // пишем резолв до мержа; отказ записи — bounce (канон
                // UC-01, авторемонт без мержа); мерж по
                // probe-верифицированному URL — мержится ровно то, что
                // прощупано.
                let url = match probe.url.as_deref() {
                    Some(u) => u,
                    None => {
                        // Defensive: на текущих гейтвеях недостижимо
                        // (Open = вердикт о конкретном PR) — текст без
                        // «not found», чтобы не лгал о причине
                        self.bounce(board, task, "probe resolved no PR URL", out)
                            .await?;
                        return Ok(false);
                    }
                };
                if let Err(e) = self.fill_cell_if_differs(board, task, url).await {
                    self.bounce(board, task, &format!("PR cell fill failed: {e}"), out)
                        .await?;
                    return Ok(false);
                }
                match self.prs.merge(&board.repo, url).await {
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
                        self.bounce(board, task, &format!("merge failed: {e}"), out)
                            .await?;
                        Ok(false)
                    }
                }
            }
        }
    }

    /// Дозаполнение canonical PR-ячейки, если она расходится с резолвом
    /// (#1519; ревью Low раунда 3 — единая точка «записать, если
    /// расходится» вместо дубля в Merged/Open ветках): живая ячейка не
    /// переписывается, запись логируется. Err — отказ записи, решение
    /// (bounce/warn) за вызывающей веткой. URL резолва парсится здесь
    /// один раз — дальше в порты идёт доменный \`&PrUrl\`.
    async fn fill_cell_if_differs(
        &self,
        board: &BoardPlan,
        task: &Task,
        url: &str,
    ) -> UsecaseResult<()> {
        if task.pr().map(|u| u.as_str()) == Some(url) {
            return Ok(());
        }
        let parsed = uwuwu_domain::PrUrl::parse(url)
            .ok_or_else(|| UsecaseError::Board(format!("PR «{url}» is not an http(s) URL")))?;
        self.board
            .set_pr(&board.board, task.number(), &parsed)
            .await?;
        tracing::info!(
            event = "automerge.pr_cell_filled",
            number = task.number(),
            url,
            "Automerge: canonical PR-ячейка дозаполнена резолвом (#1519)"
        );
        Ok(())
    }

    /// Bounce в In Progress с комментом-причиной (канон UC-01) — единая
    /// точка инкремента `automerge_bounced` (#1519, ревью Low: правило
    /// «семантический bounce = +1» живёт здесь, не на сайтах вызова).
    async fn bounce(
        &self,
        board: &BoardPlan,
        task: &Task,
        why: &str,
        out: &mut TickOutcome,
    ) -> UsecaseResult<()> {
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
        out.automerge_bounced += 1;
        Ok(())
    }
}

/// Тайтл конвейерной сессии: `#<номер> <эмодзи стадии> <тайтл>` (задача
/// #1212); единая точка сборки — bootstrap наследует тем же значением.
fn session_title(task: &Task, slot: Slot) -> String {
    format!("#{} {} {}", task.number(), slot.emoji(), task.title())
}

/// Маркеры слотов в spawn_prompt — контракт мок-стенда:
/// `mock_opencode::stage_action_of_prompt` классифицирует спавн по этим
/// фрагментам (импортирует константу). Методика стадии живёт в дефе агента;
/// сервер шлёт данные, маркер и указатель, а контракт хода (UC-11) и
/// bootstrap-подсказки (UC-05) остаются в этом промпте.
pub const STAGE_PROMPTS: &[(Slot, &str)] = &[
    (Slot::Design, "stage=developer/design"),
    (Slot::Dev, "stage=developer/dev"),
    (Slot::PlanReview, "stage=plan-reviewer"),
    (Slot::ResultReview, "stage=code-quality-reviewer"),
    (Slot::Test, "stage=tester"),
    (Slot::TestReview, "stage=test-reviewer"),
];

pub(crate) fn spawn_prompt(board: &BoardPlan, task: &Task, slot: Slot) -> String {
    let stage = STAGE_PROMPTS
        .iter()
        .find(|(s, _)| *s == slot)
        .map(|(_, m)| *m)
        .unwrap_or_default();
    let agent = Agent::of_slot(slot)
        .map(naming::agent_name)
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
         Methodology: agents/{agent}.md (loaded by opencode as your agent definition).\n\
         \n\
         ⚠ Turn contract: your session SLEEPS after the reply — the pipeline will not\n\
         continue until you close the transaction. Do not end the turn without one of:\n\
         1) a status transition (uwuwu_task_edit {{number, status}}): {finish};\n\
         2) a question to the owner in a comment (uwuwu_task_comment) + status\n\
         «Needs attention».\n\
         «Worked and went silent» = the row hangs until the pipeline auto-ping\n\
         (15 min after a finished turn, up to 60 min while the turn is in progress).\n\
         \n\
         After the owner answers a question in Needs attention: fix the verbatim\n\
         reply in the body or thread, then call uwuwu_task_edit with owner_answer\n\
         set to that reply and the source status — the gate requires it.\n\
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
        agent = agent,
    )
}

/// Бюджет команды task-env (мс): подготовка env одного спавна укладывается —
/// носитель числа, бывший общий таймаут клиента адаптера (opencode.rs);
/// батч env-gc носит свой бюджет прохода (env_gc.rs). #1526.
pub const TASK_ENV_COMMAND_BUDGET_MS: u64 = 20_000;
