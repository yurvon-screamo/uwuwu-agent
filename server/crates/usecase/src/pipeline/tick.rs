//! Тик конвейера: чтение досок → домен решает → гейты → исполнение
//! через порты. Оркестрация знает слова (логи/промпты) — домен нет.

use super::actions::WakeAct;
use crate::error::{UsecaseError, UsecaseResult};
use crate::naming;
use crate::ports::{Clock, Liveness, PrGateway, TaskBoardRepo, TickStateStore};
use crate::router::SessionRouter;
use crate::spawn_models::SpawnModels;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use uwuwu_domain::{
    decide, gate, spawn_guard, Action, Awaits, Gate, LoopOutcome, Priority, ReviewRoundStep,
    SessionSighting, Slot, SpawnGuard, SpawnLoop, SpawnPolicy, Status, Task, REVIEW_ROUND_CAP,
};

/// Режим конвейера: observe — только решения в лог; live — исполнение.
/// Кулдаун спавна слота строки: 15 минут (анти-шторм любого происхождения).
pub const SPAWN_COOLDOWN_MS: u64 = 15 * 60_000;

/// Сталл-детект: stage-сессия жива, но ход завершён (тишина) и статус
/// не переведён — строка висит до внешнего толчка (паттерн 16.11:
/// #1151, #1135, #384 — агент заканчивает ход без статусной транзакции).
/// Автопинг через час молчания, повтор через два, затем Needs attention.
pub const STALL_SILENCE_MS: u64 = 60 * 60_000;
pub const STALL_PING_EVERY_MS: u64 = 2 * 60 * 60_000;
pub const STALL_MAX_PINGS: u32 = 2;
/// Мягкий пинг (санкция владельца 16.11): ход завершён (idle), транзакция
/// не закрыта — прямой промпт в сессию через 15 мин, повтор через 30.
pub const STALL_SOFT_SILENCE_MS: u64 = 15 * 60_000;
pub const STALL_SOFT_EVERY_MS: u64 = 30 * 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Observe,
    Live,
}

/// Доска в плане тика: всё, что нужно оркестрации об окружении проекта.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardPlan {
    pub project: String,
    pub board: String,
    /// Репо проекта (PR-пробы/мерж).
    pub repo: String,
    /// Мастер-чекаут проекта на исполнителе (cwd bootstrap-сессии).
    pub checkout: String,
    /// Корень env задач на исполнителе (~/agents-env).
    pub env_root: String,
}

impl BoardPlan {
    /// Env-директория строки: `<env_root>/<row>` — единственная формула
    /// для спавна и env-гарда (ревью Low #1163: расхождение копий =
    /// тихая слепота гарда).
    pub fn env_repo_dir(&self, number: u64) -> String {
        format!("{}/{}", self.env_root, number)
    }
}

/// План тика: режим + политика + доски.
#[derive(Debug, Clone)]
pub struct TickPlan {
    pub mode: Mode,
    pub policy: SpawnPolicy,
    pub boards: Vec<BoardPlan>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TickOutcome {
    pub actions_failed: usize,
    pub rows_total: usize,
    pub spawns_allowed: usize,
    pub spawns_done: usize,
    pub held_cup: usize,
    pub deferred_peak: usize,
    pub observed: usize,
    pub wakeups: usize,
    /// Будильников/респавнов, отложенных политикой (пик/CUP — #1200).
    pub wakeups_deferred: usize,
    /// Мержей автомержа: строка остаётся в Review (#1232).
    pub automerged: usize,
    /// Строк, переведённых в Needs attention спавн-петлёй.
    pub attention_flagged: usize,
    /// Автопингов сталл-детекта (ход завершён, статус не переведён).
    pub stall_pings: usize,
    /// Мягких пингов (idle-сессия, прямой промпт — санкция владельца 16.11).
    pub soft_pings: usize,
    /// Строк, переведённых в Needs attention сталл-детектом.
    pub stall_flagged: usize,
    /// Строк, снятых с раунда капом ревью-раундов (#1311).
    pub review_flagged: usize,
    /// Busy-сессии исполнителя по union-скану на старте тика (или
    /// статусный фолбэк-подсчёт при недоступности скана) — наблюдаемость
    /// единого капа (#1231).
    pub sessions_busy: usize,
    /// Спавнов, удержанных гардом «одна живая сессия на строку» (#1163).
    pub held_live: usize,
    /// Спавнов, удержанных слепотой: список исполнителя проекта недоступен,
    /// спавн без зрения невозможен (#1263).
    pub held_blind: usize,
    /// Гашений low/empty-активов при отдаче слота кандидату (#1259).
    pub preempts: usize,
    /// Строк, пропущенных тиком при отказе enrichment (#1163): решение
    /// по невидимым ячейкам — источник дублей, ждём следующий тик.
    pub enrich_skipped: usize,
    /// Строк с недостоверной пробой активности исполнителя (Err): отказ
    /// сталл-прохода строки — без эскалации и записи каунтеров (#1281).
    pub probe_failed: usize,
}

/// Результат прохода сталл-детекта по строке.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StallAct {
    None,
    Pinged,
    /// Мягкий пинг: ход завершён (idle), прямой промпт в сессию.
    SoftPinged,
    Flagged,
    /// Недостоверная проба исполнителя: сталл-проход строки отложен —
    /// вердикт о тишине не может быть получен (#1281, срез честности
    /// пробы). Эскалация и запись каунтеров не выполняются.
    ProbeFailed,
}

/// Полоса отказов пробы за тик, при которой тик сигнализирует деградацию
/// пробы одним громким событием (прецедент ограниченной деградации
/// ADR-086); обработка остальных строк не останавливается — изоляция
/// построчная (#1281).
pub const PROBE_DEGRADE_MIN_ROWS: usize = 5;

/// Оркестратор тика: порты внедряются извне (композиция в uwuwu-server).
pub struct TickUseCase {
    pub board: Arc<dyn TaskBoardRepo>,
    /// Роутер «проект → executor»: спавны/будильники — гейтвей проекта.
    pub sessions: Arc<SessionRouter>,
    pub prs: Arc<dyn PrGateway>,
    pub clock: Arc<dyn Clock>,
    pub state: Arc<dyn TickStateStore>,
    pub models: Arc<SpawnModels>,
    /// ms старта процесса сервера: эскалация сталла требует пингов,
    /// доставленных в текущем процессе — раунд из-под рестарта
    /// недействителен (инцидент 18.11 #1281: persisted каунтеры volume
    /// переживают пересоздание контейнера, парковка по чужому раунду).
    pub boot_ms: u64,
}

/// Причина hold спавна: зрение недоступно или в env живая сестра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpawnHold {
    /// Список исполнителя проекта недоступен — спавн без зрения невозможен,
    /// hold до восстановления скана (#1263).
    BlindScan,
    /// Живая сессия той же семьи в env строки (#1163).
    Live,
}

/// Кандидат-жертва преемпшена (#1259): busy low/empty-строка доски, чей
/// слот CUP может быть отдан кандидату строже нижнего ранга. Не-строки
/// (кроны/боты/ручные сессии) в пул не попадают структурно — отбор идёт
/// только по строкам досок.
#[derive(Debug, Clone)]
pub(crate) struct PreemptCandidate {
    /// Проект-исполнитель жертвы (abort/delete — через её гейтвей).
    pub project: String,
    pub number: u64,
    pub session: String,
    pub priority: Priority,
    /// ms спавна слота жертвы: детерминизм выбора (свежее — раньше).
    pub spawned_ms: u64,
}

/// Пул жертв тика: собирается в run() из union-скана, расходуется
/// HeldCup-ветками run_board (по одной жертве на отдачу слота).
/// Mutex — не для параллелизма (доски обходятся последовательно):
/// interior mutability для изъятия жертв между досками сквозь await
/// и Send-ность будущего тика.
#[derive(Default)]
pub(crate) struct PreemptPool {
    candidates: Mutex<Vec<PreemptCandidate>>,
}

impl PreemptPool {
    fn push(&self, c: PreemptCandidate) {
        self.candidates.lock().unwrap().push(c);
    }

    /// Взять жертву для кандидата: строго ниже рангом; среди подходящих —
    /// минимальный ранг, при равенстве — свежее заспавненная (контекст
    /// младше — дешевле потерять). Достаёт из пула (повторное гашение
    /// одной жертвы в тике невозможно).
    fn take(&self, waiter: &Task) -> Option<PreemptCandidate> {
        let mut cands = self.candidates.lock().unwrap();
        let idx = cands
            .iter()
            .enumerate()
            .filter(|(_, c)| uwuwu_domain::can_preempt(waiter.priority(), c.priority))
            .min_by_key(|(_, c)| {
                (
                    std::cmp::Reverse(c.priority.rank()),
                    std::cmp::Reverse(c.spawned_ms),
                )
            })
            .map(|(i, _)| i)?;
        Some(cands.remove(idx))
    }
}

/// Контекст строки тика: данные union-скана и enrichment, общие для
/// прохода доски (#1163). Не решение — вход решений.
pub(crate) struct RowContext<'a> {
    /// Слепки сессий исполнителя ЭТОГО проекта (пер-проектное зрение,
    /// #1263); None — скан своего исполнителя недоступен, спавн держится
    /// до восстановления (#1263 — инверсия never-worse: слепой спавн
    /// поверх живой сестры = класс инцидента #1241).
    pub(crate) sightings: Option<&'a [SessionSighting]>,
    /// Строки с отказом enrichment — тиком пропускаются.
    pub(crate) enrich_failed: &'a HashSet<u64>,
    /// Кандидаты-жертвы преемпшена (#1259): busy low/empty-активы.
    pub(crate) preempts: &'a PreemptPool,
}

impl TickUseCase {
    /// Вердикт hold спавна слота (#1163 + #1263): слепой скан — hold без
    /// зрения; живая сессия семьи в env строки — hold до её финала. None —
    /// препятствий нет. Second source (sighting_known) — отдельная проверка
    /// вызывающего, сюда не входит.
    pub(crate) fn spawn_hold(
        &self,
        ctx: &RowContext<'_>,
        board: &BoardPlan,
        task: &Task,
        slot: Slot,
        now_ms: u64,
    ) -> Option<SpawnHold> {
        let Some(list) = ctx.sightings else {
            return Some(SpawnHold::BlindScan);
        };
        if spawn_guard(list, &board.env_repo_dir(task.number()), slot, now_ms) == SpawnGuard::Hold {
            Some(SpawnHold::Live)
        } else {
            None
        }
    }

    pub async fn run(&self, plan: &TickPlan) -> UsecaseResult<TickOutcome> {
        let mut out = TickOutcome::default();
        // CUP-кап глобальный по проектам (канон #1133): активные строки
        // считаются по всем доскам до обхода, не per-board
        let mut boards = Vec::with_capacity(plan.boards.len());
        for board in &plan.boards {
            let tasks = self.board.read_board(&board.board).await?;
            out.rows_total += tasks.len();
            boards.push((board, tasks));
        }
        // Enrichment: snapshot Projects v2 не несёт глобальных полей issue
        // (сессии/ветка/PR) — строки действующих стадий дотягиваем одним
        // REST-зовом каждая. Без этого тик не видит сессию стадии:
        // wakeup недостижим, needs_spawn вечен → спавн-дубли и недоставленные
        // комменты (инцидент ночи 15-16.11). Отказ enrichment (#1163):
        // строка пропускается тиком — решение по невидимым ячейкам
        // (сессия всегда None) и есть механизм дубль-спавнов.
        let mut enrich_failed: HashSet<u64> = HashSet::new();
        for (bp, tasks) in boards.iter_mut() {
            for t in tasks.iter_mut() {
                // stage-строки (wakeup/needs_spawn) + Review-строки
                // (автомерж: PR/ветка — глобальные поля, без enrichment
                // автомерж слеп — PR #338/#339 висели немержеными)
                if t.stage_slot().is_some() || t.status() == Status::UserReview {
                    match self.board.issue_fields(t.number()).await {
                        Ok(f) => t.apply_fields(f),
                        Err(e) => {
                            tracing::warn!(
                                event = "enrich.failed",
                                number = t.number(),
                                error = %e,
                                "issue_fields enrichment — строка пропущена до следующего тика"
                            );
                            enrich_failed.insert(t.number());
                        }
                    }
                }
            }
            let _ = bp;
        }
        // CUP единый (#1231) поверх пер-исполнительного зрения (#1263):
        // union-скан одним проходом — busy-подсчёт CUP + слепки гарда
        // «одна живая сессия на строку». Старый подсчёт (outcome-фильтр
        // «живых» + per-session idle-пробы) терял сессии на 2+ ходе:
        // outcome — атрибут последнего завершённого хода и при новом ходе
        // не очищается (разбор #1231: 12 активных при cap=7). В union
        // входят все источники: стадии, сайдкары, кроны, боты, ручные
        // сессии владельца — единый кап на исполнителя.
        // Strict для CUP (#1203/#1231): отказ ЛЮБОГО исполнителя → оба
        // источника None — частичный список исказил бы busy-подсчёт и
        // спрятал живую сироту от гарда (дубль-спавн); фолбэк — статусный
        // подсчёт, консервативно против спавн-шторма (спавнить некому
        // всё равно). Гард спавна читает список СВОЕГО исполнителя
        // (sight_by_executor): отказ чужого зрение проекта не гасит
        // (#1263). Кандидаты без сессии — очередь, не нагрузка (#1244).
        let per_exec = self.sessions.list_sessions_per_executor().await;
        let all_ok = per_exec.iter().all(|(_, r)| r.is_ok());
        let scan: Option<Vec<crate::ports::SessionInfo>> = if all_ok {
            Some(
                per_exec
                    .iter()
                    .filter_map(|(_, r)| r.as_ref().ok())
                    .flatten()
                    .cloned()
                    .collect(),
            )
        } else {
            for (name, r) in &per_exec {
                if let Err(e) = r {
                    tracing::warn!(event = "cup.list_sessions_failed", executor = %name,
                        error = %e, "list_sessions исполнителя недоступен — CUP по статусам");
                }
            }
            None
        };
        // Пер-проектное зрение: слепки по исполнителям, доска получает свой
        let mut sight_by_executor: HashMap<String, Option<Vec<SessionSighting>>> = HashMap::new();
        for (name, r) in &per_exec {
            let list = r.as_ref().ok().map(|sessions| {
                sessions
                    .iter()
                    .map(|s| SessionSighting {
                        id: s.id.clone(),
                        directory: s.directory.clone(),
                        agent: s.agent.clone(),
                        outcome: s.outcome.clone(),
                        updated_ms: s.updated_ms,
                    })
                    .collect()
            });
            sight_by_executor.insert(name.clone(), list);
        }
        let mut fallback_active = 0usize;
        if scan.is_none() {
            for (_, tasks) in boards.iter() {
                for t in tasks.iter() {
                    if t.status().is_active() {
                        fallback_active += 1; // сессионные и кандидаты — консервативно
                    }
                }
            }
        }
        // Пул жертв преемпшена (#1259) на #1231-базе: busy берётся из
        // union-скана (SessionInfo.busy), не пер-сессионными пробами.
        // Blind/fallback (scan None) пул не наполняет — гасить без зрения
        // нельзя (#1263); idle-жертвы не попадают (капа не держат);
        // high/medium не гасятся никогда. Не-строки (кроны/боты/ручные)
        // вне пула структурно — отбор только по строкам досок.
        let preempt_pool = PreemptPool::default();
        if let Some(list) = &scan {
            for (bp, tasks) in boards.iter() {
                for t in tasks.iter() {
                    if !t.status().is_active() {
                        continue;
                    }
                    let Some(slot) = t.stage_slot() else { continue };
                    let Some(session) = t.session_of(slot) else {
                        continue;
                    };
                    let Some(info) = list.iter().find(|s| s.id == *session) else {
                        continue; // мёртвая — гасить нечего
                    };
                    if !info.busy {
                        continue; // idle не держит кап
                    }
                    if !matches!(t.priority(), Priority::Low | Priority::Empty) {
                        continue;
                    }
                    let spawned_ms = self
                        .state
                        .last_spawn_ms(&bp.board, t.number(), slot)
                        .unwrap_or(0);
                    preempt_pool.push(PreemptCandidate {
                        project: bp.project.clone(),
                        number: t.number(),
                        session: session.to_string(),
                        priority: t.priority(),
                        spawned_ms,
                    });
                }
            }
        }
        let global_active: usize = match &scan {
            Some(list) => list.iter().filter(|s| s.busy).count(),
            None => fallback_active,
        };
        out.sessions_busy = global_active;
        // Счётчик капа — один на весь тик, общий для всех досок (#1244):
        // спавн одной доски занимает слот глобального капа, следующая
        // доска видит занятость через ссылку
        let mut active_now = global_active;
        for (board, tasks) in boards {
            // Пер-проектное зрение (#1263): контекст доски — список её
            // исполнителя; чужие исполнители зрение проекта не гасят.
            let executor = self.sessions.project_executor_name(&board.project);
            let sightings = sight_by_executor.get(executor).and_then(|v| v.as_deref());
            out = self
                .run_board(
                    plan,
                    board,
                    tasks,
                    &mut active_now,
                    &RowContext {
                        sightings,
                        enrich_failed: &enrich_failed,
                        preempts: &preempt_pool,
                    },
                    out,
                )
                .await?;
        }
        if let Err(e) = self.state.save() {
            tracing::error!(event = "state.save_failed", error = %e, "Failed to save tick state");
        }
        // Полоса отказов пробы за тик — одно громкое событие деградации
        // (массовый Err = класс гейт-окна #1160); обработка строк при этом
        // не останавливалась — изоляция построчная (#1281).
        if out.probe_failed >= PROBE_DEGRADE_MIN_ROWS {
            tracing::error!(
                event = "stall.probe_degraded",
                probe_failed = out.probe_failed,
                "Проба активности исполнителя деградировала: массовый отказ сталл-проб — вердикты тишины недостоверны"
            );
        }
        tracing::info!(
            event = "tick.completed",
            rows = out.rows_total,
            spawns = out.spawns_done,
            held_cup = out.held_cup,
            sessions_busy = out.sessions_busy,
            preempts = out.preempts,
            deferred_peak = out.deferred_peak,
            wakeups = out.wakeups,
            wakeups_deferred = out.wakeups_deferred,
            automerged = out.automerged,
            held_live = out.held_live,
            stall_pings = out.stall_pings,
            soft_pings = out.soft_pings,
            stall_flagged = out.stall_flagged,
            review_flagged = out.review_flagged,
            observed = out.observed,
            spawns_allowed = out.spawns_allowed,
            enrich_skipped = out.enrich_skipped,
            probe_failed = out.probe_failed,
            mode = ?plan.mode,
            failed = out.actions_failed,
            "Tick completed"
        );
        Ok(out)
    }

    async fn run_board(
        &self,
        plan: &TickPlan,
        board: &BoardPlan,
        mut tasks: Vec<Task>,
        active_now: &mut usize,
        ctx: &RowContext<'_>,
        mut out: TickOutcome,
    ) -> UsecaseResult<TickOutcome> {
        let now = self.clock.utc_minute();
        let now_ms = self.clock.now_ms();
        // Активные для гейтов: снапшот обхода + состоявшиеся спавны
        // этого тика, общий для всех досок (ссылка из run, #1244) —
        // иначе несколько мёртвых stage-строк пройдут CUP по одному
        // снапшоту и оверселят кап внутри прохода (Low-находка #1200),
        // а много-досочные тики — по разу на доску.
        // Дебаг-гейт одной строки (мок-Е2Е): UWUWU_ONLY_TASK=<number> —
        // конвейер действует только по ней, остальная доска read-only
        if let Ok(only) = std::env::var("UWUWU_ONLY_TASK") {
            if let Ok(n) = only.trim().parse::<u64>() {
                tasks.retain(|t| t.number() == n);
            }
        }
        tasks.sort_by_key(|t| t.priority().rank());
        for task_ref in &tasks {
            // Отказ enrichment (#1163): строка пропускается — ячейки не
            // видны, decide по снапшоту даст вечный needs_spawn (механизм
            // дубль-спавнов ночи 15.11). Пропущенный тик ретраится даром.
            if ctx.enrich_failed.contains(&task_ref.number()) {
                out.enrich_skipped += 1;
                continue;
            }
            let mut cleared_clone: Option<Task> = None;
            // Attention-механика: счётчики слотов, сохранённые при другом
            // статусе строки, устарели (строка ушла и вернулась) — сброс.
            // Live-only: observe стейт не пишет.
            if plan.mode == Mode::Live {
                self.state.reset_stale_spawn_loops(
                    &board.board,
                    task_ref.number(),
                    task_ref.status(),
                );
                // Канон сессий: переход стадии = новый раунд слота. Ревью/тест —
                // свежие глаза всегда (незамыленность оценки); дев-семья едина
                // между этапами и откатами (требование владельца), но мёртвая
                // дев-ячейка не «едина» ни с чем и замораживает кик владельца
                // навсегда (#397) — канон fast-track: исключение от спавна —
                // живая дев-ячейка. Какой слот чистим — решает хелпер.
                let prev = self.state.last_seen_status(&board.board, task_ref.number());
                if prev.as_ref() != Some(&task_ref.status()) {
                    cleared_clone = self.round_transition(board, task_ref, &mut out).await;
                    self.state.set_last_seen_status(
                        &board.board,
                        task_ref.number(),
                        task_ref.status(),
                    );
                    // Цикл-гвард ревью-контура (#1311): счёт подряд
                    // request_changes по статус-переходам; кап достигнут —
                    // эскалация вместо раунда N+1, строка снята с тика.
                    if self
                        .review_round_pass(board, prev, task_ref, &mut out)
                        .await
                    {
                        continue;
                    }
                }
            }
            // тень: если слот снят переходом — работаем с очищенным клоном
            let task: &Task = cleared_clone.as_ref().unwrap_or(task_ref);
            let action = decide(task);
            match action {
                Action::Spawn { slot } => {
                    // Анти-шторм: слот строки не спавнится чаще кулдауна
                    let cooldown_left = {
                        let last = self.state.last_spawn_ms(&board.board, task.number(), slot);
                        last.map(|ts| self.clock.now_ms().saturating_sub(ts))
                            .unwrap_or(u64::MAX)
                    };
                    if cooldown_left < SPAWN_COOLDOWN_MS {
                        out.observed += 1;
                        continue;
                    }
                    // Attention-механика: >2 спавнов слота при неизменном
                    // статусе = петля → Needs attention, конвейер бросает строку
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
                        out.attention_flagged += 1;
                        tracing::warn!(
                            event = "spawn.loop_escalated",
                            number = task.number(),
                            slot = naming::slot_key(slot),
                            "Spawn loop >2: Needs attention"
                        );
                        if plan.mode == Mode::Live {
                            let _ = self
                                .board
                                .set_status(&board.board, task.number(), Status::NeedsAttention)
                                .await;
                            let _ = self
                                .board
                                .post_comment(
                                    task.number(),
                                    &format!(
                                        "⚠︎ Спавн-петля: слот `{}` спавнился >2 раз без \
                                         прогресса статуса ({:?}). Строка переведена в \
                                         Needs attention — разбор владельца.",
                                        naming::slot_key(slot),
                                        task.status()
                                    ),
                                )
                                .await;
                        }
                        continue;
                    }
                    // Гард «одна живая сессия на строку» (#1163) + зрение
                    // (#1263): сироты спавна, клеймы в гонке с тиком и
                    // невидимые ячейки не дублируют раунд; слепой скан —
                    // hold без спавна до восстановления списка.
                    match self.spawn_hold(ctx, board, task, slot, now_ms) {
                        Some(SpawnHold::Live) => {
                            out.held_live += 1;
                            tracing::info!(
                                event = "spawn.held_live_session",
                                number = task.number(),
                                slot = naming::slot_key(slot),
                                "Spawn held: живая сессия в env строки (second source #1163)"
                            );
                            continue;
                        }
                        Some(SpawnHold::BlindScan) => {
                            out.held_blind += 1;
                            tracing::info!(
                                event = "spawn.held_blind_scan",
                                number = task.number(),
                                slot = naming::slot_key(slot),
                                "Spawn held: список исполнителя недоступен — спавн без зрения невозможен (#1263)"
                            );
                            continue;
                        }
                        None => {}
                    }
                    let mut gated = gate(task, &action, plan.policy, *active_now, now);
                    if matches!(gated, Gate::HeldCup { .. }) && plan.mode == Mode::Live {
                        // Отдача слота при понижении приоритета (#1259):
                        // спавн кандидата держит кап, есть busy-жертва
                        // строго ниже рангом → гасим её и отдаём слот на
                        // этом же тике (детали — в try_preempt).
                        if self
                            .try_preempt(board, task, ctx, active_now, &mut out)
                            .await
                        {
                            gated = gate(task, &action, plan.policy, *active_now, now);
                        }
                    }
                    match gated {
                        Gate::Allowed => {
                            out.spawns_allowed += 1;
                            if plan.mode == Mode::Live {
                                match self.spawn_stage(board, task, slot).await {
                                    Ok(session) => {
                                        *active_now += 1;
                                        // счётчик/кулдаун пишем только при
                                        // реальном спавне: observe не накручивает
                                        // петли (#1046), а временный отказ
                                        // (бюджет 10/ч) — не петля и не спавн
                                        // (15.11: #1151 high ложно эскалировала
                                        // бы за 3 бюджет-отказа)
                                        self.state.set_spawn_loop(
                                            &board.board,
                                            task.number(),
                                            slot,
                                            guard.status(),
                                            guard.repeats(),
                                        );
                                        self.state.mark_spawn(
                                            &board.board,
                                            task.number(),
                                            slot,
                                            self.clock.now_ms(),
                                        );
                                        if let Err(e) = self
                                            .board
                                            .set_session(
                                                &board.board,
                                                task.number(),
                                                slot,
                                                &session,
                                            )
                                            .await
                                        {
                                            out.actions_failed += 1;
                                            // Сирота (#1163): сессия создана и
                                            // запромпчена, ячейки нет — гасим,
                                            // иначе живой сеанс вне ячейки
                                            // вечно держит гард, а будить его
                                            // некому (id не в ячейке).
                                            let _ = self
                                                .sessions
                                                .project(&board.project)
                                                .delete_session(&session)
                                                .await;
                                            self.log_action_failed(task_ref, e);
                                        } else {
                                            out.spawns_done += 1;
                                        }
                                    }
                                    Err(e) => {
                                        out.actions_failed += 1;
                                        self.log_action_failed(task_ref, e);
                                    }
                                }
                            }
                        }
                        Gate::HeldCup { active, cap } => {
                            out.held_cup += 1;
                            tracing::info!(
                                event = "spawn.held_cup",
                                number = task.number(),
                                active,
                                cap,
                                "Spawn held: CUP cap"
                            );
                        }
                        Gate::DeferredPeak => {
                            out.deferred_peak += 1;
                            tracing::info!(
                                event = "spawn.deferred_peak",
                                number = task.number(),
                                "Spawn deferred: peak window"
                            );
                        }
                    }
                }
                Action::Automerge => {
                    if plan.mode == Mode::Live {
                        let merged = self
                            .automerge(board, task)
                            .await
                            .map_err(|e| self.log_action_failed(task, e))?;
                        // счётчик — факты мержа: bounce/pending/no-op не считаются
                        if merged {
                            out.automerged += 1;
                        }
                    } else {
                        tracing::info!(
                            event = "automerge.probe",
                            number = task.number(),
                            pr = task.pr().map(|u| u.as_str()).unwrap_or(""),
                            "Automerge probe (observe)"
                        );
                    }
                }
                Action::Observe { awaits } => {
                    if plan.mode == Mode::Live && awaits == Awaits::StageSession {
                        self.observe_stage_session(board, plan, task, active_now, ctx, &mut out)
                            .await;
                    } else {
                        out.observed += 1;
                    }
                }
            }
        }
        Ok(out)
    }

    /// Отдача слота при понижении приоритета (#1259): кандидат держится
    /// капом, в пуле есть busy-жертва строго ниже рангом → гасим её
    /// (abort+delete) и освобождаем слот на этом же тике. Возвращает
    /// true, если слот освобождён (кандидату нужен повторный гейт).
    /// Готовность кандидата ДО гашения: бюджетная проба (жертва зря не
    /// гибнет); кулдаун/луп-гард/гард «одна живая» уже пройдены выше по
    /// коду тика. Пик-окно сюда не доходит: DeferredPeak проверяется
    /// гейтом раньше CUP — не-high кандидаты жертв не гасят.
    /// Отказ гашения — громкий event, ретрай следующим тиком (тихого
    /// no-op нет: abort-first с фоллбеком в гейтвее, см. opencode.rs).
    async fn try_preempt(
        &self,
        board: &BoardPlan,
        task: &Task,
        ctx: &RowContext<'_>,
        active_now: &mut usize,
        out: &mut TickOutcome,
    ) -> bool {
        if !self.sessions.project(&board.project).spawn_allowed() {
            tracing::info!(
                event = "cup.preempt.budget_held",
                number = task.number(),
                "CUP: спавн-бюджет исчерпан — гашения нет (жертва зря не гибнет)"
            );
            return false;
        }
        let Some(victim) = ctx.preempts.take(task) else {
            // Причины пустого пула для разбора: fallback-режим (скана нет
            // — пул структурно пуст), в скане нет busy low/empty-строк,
            // либо ранги не строго ниже (жертвы-low заняты равным/
            // высшим кандидатом или уже изъяты этим тиком).
            tracing::info!(
                event = "cup.preempt.no_victim",
                number = task.number(),
                active = *active_now,
                "CUP: жертвы в пуле нет — слот не отдаётся"
            );
            return false;
        };
        let victim_gw = self.sessions.project(&victim.project);
        let aborted = victim_gw.abort_session(&victim.session).await;
        let deleted = if aborted.is_ok() {
            victim_gw.delete_session(&victim.session).await
        } else {
            Err(UsecaseError::Session(
                "abort не прошёл — delete не вызывается".into(),
            ))
        };
        match (aborted, deleted) {
            (Ok(()), Ok(())) => {
                // Burn-down корректен: жертва попала в пул только из
                // busy-учёта фазы 2 (idle не держат кап и в пул не
                // попадают) — вычёт одного слота ровно соответствует
                // погашенной трате; сатуратинг страховка от рассинхрона
                // снапшота и счётчика (спавны тика поверх busy).
                *active_now = active_now.saturating_sub(1);
                out.preempts += 1;
                tracing::info!(
                    event = "cup.preempt",
                    waiter = task.number(),
                    victim = victim.number,
                    victim_session = %victim.session,
                    "CUP: слот отдан — low-актив погашен (abort+delete), возобновление штатным respawn'ом"
                );
                true
            }
            (abort_r, delete_r) => {
                out.actions_failed += 1;
                tracing::warn!(
                    event = "cup.preempt.failed",
                    waiter = task.number(),
                    victim = victim.number,
                    abort = ?abort_r.err(),
                    delete = ?delete_r.err(),
                    "CUP: гашение жертвы не удалось — кандидат остаётся held, ретрай следующим тиком"
                );
                false
            }
        }
    }

    /// Плечо Observe стадии: будильник/респавн под политикой запуска
    /// (UC-05: пик держит не-high и будильник, и респавн; CUP держит
    /// респавн — как прямой спавн), при тишине — проход сталл-детекта.
    /// Респавн инкрементит active_now (фактический спавн в этом тике).
    /// Сбой логируется в outcome.
    async fn observe_stage_session(
        &self,
        board: &BoardPlan,
        plan: &TickPlan,
        task: &Task,
        active_now: &mut usize,
        ctx: &RowContext<'_>,
        out: &mut TickOutcome,
    ) {
        match self
            .wakeup_if_fresh(board, plan.policy, task, *active_now, ctx)
            .await
        {
            Ok(WakeAct::Woke) => {
                out.wakeups += 1;
                // доставленный ход занимает слот капа в этом же тике (#1231)
                *active_now += 1;
            }
            Ok(WakeAct::Respawned) => {
                out.spawns_done += 1;
                *active_now += 1;
            }
            Ok(WakeAct::Deferred) => out.wakeups_deferred += 1,
            Ok(WakeAct::Guarded) => out.held_live += 1,
            Ok(WakeAct::GuardedBlind) => out.held_blind += 1,
            Ok(WakeAct::None) => match self.stall_pass(board, task).await {
                Ok(StallAct::None) => {}
                Ok(StallAct::Pinged) => out.stall_pings += 1,
                Ok(StallAct::SoftPinged) => out.soft_pings += 1,
                Ok(StallAct::Flagged) => out.stall_flagged += 1,
                Ok(StallAct::ProbeFailed) => out.probe_failed += 1,
                Err(e) => {
                    out.actions_failed += 1;
                    self.log_action_failed(task, e);
                }
            },
            Err(e) => {
                out.actions_failed += 1;
                self.log_action_failed(task, e);
            }
        }
    }

    /// Цикл-гвард ревью-контура (#1311): статус-переход классифицирует
    /// домен, счётчик — персистентный state тика. Считаются переходы
    /// «ревьювер вернул строку в работу» (ReviewResult→InProgress /
    /// ReviewPlan→Design — констатация request_changes без парсинга
    /// вердиктов, ADR-106); кап — REVIEW_ROUND_CAP. Кап достигнут —
    /// эскалация: Needs attention + структурный дайджест вместо раунда
    /// N+1, конвейер бросает строку. Возвращает true — строка снята
    /// с тика (вызывающему: continue).
    async fn review_round_pass(
        &self,
        board: &BoardPlan,
        prev: Option<Status>,
        task: &Task,
        out: &mut TickOutcome,
    ) -> bool {
        let rounds = self.state.review_rounds(&board.board, task.number());
        // слот последней ревью-сессии — по паре цикла счёт-перехода
        let round_slot = if prev == Some(Status::ReviewPlan) {
            Slot::PlanReview
        } else {
            Slot::ResultReview
        };
        match uwuwu_domain::review_round_step(prev, task.status(), rounds) {
            ReviewRoundStep::Count { escalate } => {
                let next = rounds + 1;
                self.state
                    .set_review_rounds(&board.board, task.number(), next);
                if !escalate {
                    return false;
                }
                out.review_flagged += 1;
                tracing::warn!(
                    event = "review.rounds_escalated",
                    number = task.number(),
                    rounds = next,
                    cap = REVIEW_ROUND_CAP,
                    "Кап ревью-раундов: подряд request_changes без approve"
                );
                // дайджест: id последней ревью-сессии из слота строки
                // (на счёт-переходе слот ревью ещё не очищен — очистка
                // идёт при ре-входе в ревью); нет сессии — честное «—»
                let last = task
                    .session_of(round_slot)
                    .map(str::to_string)
                    .unwrap_or_else(|| "—".into());
                if let Err(e) = self
                    .board
                    .set_status(&board.board, task.number(), Status::NeedsAttention)
                    .await
                {
                    // счётчик стоит на капе: эскалация ретраится следующим
                    // count-переходом (раунд N+1 пройдёт — деградация benign)
                    out.actions_failed += 1;
                    self.log_action_failed(task, e);
                    return false;
                }
                let digest = format!(
                    "⚠︎ Кап ревью-раундов: {next}-й подряд request_changes без approve \
                     (порог {REVIEW_ROUND_CAP}) — раунд {} не назначается, строка в \
                     Needs attention, разбор владельца. Находки и вердикты — в треде \
                     выше; последняя ревью-сессия: {last}.",
                    next + 1
                );
                if let Err(e) = self.board.post_comment(task.number(), &digest).await {
                    out.actions_failed += 1;
                    self.log_action_failed(task, e);
                }
                // эскалация состоялась: после разбора владельца — свежий бюджет
                self.state.set_review_rounds(&board.board, task.number(), 0);
                true
            }
            ReviewRoundStep::Reset => {
                self.state.set_review_rounds(&board.board, task.number(), 0);
                false
            }
            ReviewRoundStep::None => false,
        }
    }

    /// Смена статуса строки = новый раунд слота: определить событие
    /// (свежие глаза / кик-разморозка) и снять сессию, если раунд новый.
    /// Сбой логируется в outcome. Возвращает очищенный клон строки.
    async fn round_transition(
        &self,
        board: &BoardPlan,
        task: &Task,
        out: &mut TickOutcome,
    ) -> Option<Task> {
        let slot = task.stage_slot()?;
        let event = if matches!(slot, Slot::Design | Slot::Dev) {
            "stage.kick_unfreeze"
        } else {
            "stage.fresh_eyes"
        };
        match self.transition_clear_slot(board, task, slot, event).await {
            Ok(cleared) => cleared,
            Err(e) => {
                out.actions_failed += 1;
                self.log_action_failed(task, e);
                None
            }
        }
    }

    /// Вход строки в стадию = новый раунд слота. Ревью/тест-сессии снимаются
    /// всегда (свежие глаза, незамыленность оценки); дев-семья (Design/Dev) —
    /// только мёртвая: живая едина между этапами и откатами (требование
    /// владельца), а мёртвая блокирует разворот кика навсегда (#397) —
    /// канон fast-track: исключение от спавна — живая дев-ячейка. Unknown
    /// (serve недоступен) мёртвой не считается — консервативно, dead-детект
    /// wakeup-пути подхватит строку на следующем тике.
    /// Возвращает очищенный клон строки (тик работает с ним как с тенью).
    async fn transition_clear_slot(
        &self,
        board: &BoardPlan,
        task: &Task,
        slot: Slot,
        log_event: &'static str,
    ) -> UsecaseResult<Option<Task>> {
        let Some(session) = task.session_of(slot) else {
            return Ok(None);
        };
        if matches!(slot, Slot::Design | Slot::Dev)
            && self
                .sessions
                .project(&board.project)
                .liveness(session)
                .await
                != Liveness::Dead
        {
            return Ok(None);
        }
        self.board
            .clear_session(&board.board, task.number(), slot)
            .await?;
        let mut cleared = task.clone();
        cleared.clear_slot_session(slot);
        // новый раунд: кулдаун прошлого не переносится
        self.state.clear_spawn_ms(&board.board, task.number(), slot);
        tracing::info!(
            event = log_event,
            number = task.number(),
            slot = naming::slot_key(slot),
            "Переход стадии: сессия слота снята — раунд с новой сессией"
        );
        Ok(Some(cleared))
    }

    /// Сталл-детект: stage-сессия жива, но молчит дольше STALL_SILENCE_MS —
    /// ход завершён без статусного перевода (паттерн #1151/#1135/#384).
    /// Пинг агенту → повтор → Needs attention. Будит через механизм
    /// wakeup (пинг = коммент, следующий тик доставит его сессии).
    async fn stall_pass(&self, board: &BoardPlan, task: &Task) -> UsecaseResult<StallAct> {
        let Some(slot) = task.stage_slot() else {
            return Ok(StallAct::None);
        };
        let Some(session) = task.session_of(slot).map(str::to_string) else {
            return Ok(StallAct::None);
        };
        // нет данных/ошибка чтения — не сталл (мёртвую сессию заберёт decide);
        // тишина короче мягкого порога — оба класса молчат, idle-пробу
        // (HTTP хвоста сообщений) не тратим на свежие строки
        let silence = match self
            .sessions
            .project(&board.project)
            .last_activity_ms(&session)
            .await
        {
            Ok(Some(ms)) => self.clock.now_ms().saturating_sub(ms),
            // пустой журнал легитимной сессии — тишина 0, тихий пропуск
            Ok(None) => 0,
            // недостоверная проба (транспорт, окно гейт-заглушки #1160):
            // сталл-проход строки откладывается громко — эскалация и
            // запись каунтеров по ненадёжному вердикту недопустимы (#1281)
            Err(e) => {
                tracing::warn!(
                    event = "stall.activity_err",
                    number = task.number(),
                    error = %e,
                    "Сталл-проход отложен: проба активности исполнителя недостоверна"
                );
                return Ok(StallAct::ProbeFailed);
            }
        };
        if silence < STALL_SOFT_SILENCE_MS {
            return Ok(StallAct::None);
        }
        let idle = self.session_idle(board, &session).await;
        let (gate, every, soft) = if idle {
            (STALL_SOFT_SILENCE_MS, STALL_SOFT_EVERY_MS, true)
        } else {
            (STALL_SILENCE_MS, STALL_PING_EVERY_MS, false)
        };
        if silence < gate {
            return Ok(StallAct::None);
        }
        // раунд = статус: смена статуса обнуляет счётчик пингов
        let (prev_status, last_ping, mut count) = self
            .state
            .stall_ping(&board.board, task.number())
            .map(|(st, ms, c)| (Some(st), ms, c))
            .unwrap_or((None, 0, 0));
        if prev_status != Some(task.status()) {
            count = 0;
        }
        // Раунд из-под рестарта недействителен (инцидент 18.11 #1281):
        // persisted каунтеры volume переживают пересоздание контейнера,
        // эскалация по ним паркует живые сессии без единого доставленного
        // в этом процессе пинга. Пинги, отправленные до boot_ms, текущим
        // процессом не доставлялись — сброс персистится на носителе
        // (иначе mark_stall_ping инкрементит stale-каунтер: эскалация
        // после одного пинга, счётчик с чужими пингами).
        if count > 0 && last_ping < self.boot_ms {
            tracing::info!(
                event = "stall.round_stale",
                number = task.number(),
                "Сталл-раунд пережил рестарт — сброс на носителе, свежий раунд пингов"
            );
            self.state.clear_stall(&board.board, task.number());
            count = 0;
        }
        let now = self.clock.now_ms();
        if count > 0 && now.saturating_sub(last_ping) < every {
            return Ok(StallAct::None);
        }
        self.stall_action(board, task, slot, &session, soft, silence, count, now)
            .await
    }

    /// Действие сталл-раунда: эскалация владельцу (пинги исчерпаны),
    /// мягкий пинг прямым промптом (ход завершён) или классический
    /// коммент-автопинг (ход, возможно, идёт).
    #[allow(clippy::too_many_arguments)]
    async fn stall_action(
        &self,
        board: &BoardPlan,
        task: &Task,
        slot: Slot,
        session: &str,
        soft: bool,
        silence: u64,
        count: u32,
        now: u64,
    ) -> UsecaseResult<StallAct> {
        if count < STALL_MAX_PINGS {
            return self
                .stall_ping_round(board, task, slot, session, soft, silence, now)
                .await;
        }
        // Проба живости В МОМЕНТ эскалации (#1281, различение причин):
        // вердикт должен быть достоверным, подпись — действенной.
        // Unknown — проба недостоверна: отказ (повтор на следующем тике);
        // Dead — сессия потеряна: парковка не выполняется, dead-детект
        // wakeup-пути респавнит строку на следующем тике (ADR-107),
        // класс «сессия потеряна» живёт в комменте респавн-петли.
        match self
            .sessions
            .project(&board.project)
            .liveness(session)
            .await
        {
            Liveness::Alive => {}
            Liveness::Unknown => {
                tracing::warn!(
                    event = "stall.escalation_probe_unknown",
                    number = task.number(),
                    "Эскалация отложена: проба живости недостоверна"
                );
                return Ok(StallAct::ProbeFailed);
            }
            Liveness::Dead => {
                tracing::warn!(
                    event = "stall.escalation_session_lost",
                    number = task.number(),
                    "Эскалация отменена: сессия потеряна — строку заберёт dead-детект"
                );
                return Ok(StallAct::None);
            }
        }
        // пинги исчерпаны, сессия жива и молчит — мяч владельцу
        tracing::warn!(
            event = "stall.escalated",
            number = task.number(),
            silence_min = silence / 60_000,
            round_pings = count,
            "Сталл: агент молчит при живой сессии после раунда автопингов"
        );
        self.board
            .set_status(&board.board, task.number(), Status::NeedsAttention)
            .await?;
        self.board
            .post_comment(
                task.number(),
                &format!(
                    "⚠︎ Сталл-детект: stage-сессия `{}` молчит {} мин при живой \
                     сессии — агент не завершает статусную транзакцию (проба \
                     исполнителя: жива; автопингов в раунде: {count}). Строка в \
                     Needs attention, разбор владельца.",
                    naming::slot_key(slot),
                    silence / 60_000
                ),
            )
            .await?;
        self.state
            .mark_stall_ping(&board.board, task.number(), task.status(), now);
        Ok(StallAct::Flagged)
    }

    /// Один раунд пинга: мягкий — прямым промптом в сессию (мгновенное
    /// пробуждение, без обхода через коммент), классический — комментом.
    #[allow(clippy::too_many_arguments)]
    async fn stall_ping_round(
        &self,
        board: &BoardPlan,
        task: &Task,
        slot: Slot,
        session: &str,
        soft: bool,
        silence: u64,
        now: u64,
    ) -> UsecaseResult<StallAct> {
        if soft {
            self.sessions
                .project(&board.project)
                .prompt(
                    session,
                    &format!(
                        "⏰ Мягкий пинг: ход завершён, статусный перевод не выполнен — \
                         строка #{} стоит. Заверши транзакцию по контракту хода: \
                         uwuwu_task_edit {{number: {}, status: <следующая стадия>}} \
                         или вопрос владельцу комментом + Needs attention.",
                        task.number(),
                        task.number()
                    ),
                )
                .await?;
            self.state
                .mark_stall_ping(&board.board, task.number(), task.status(), now);
            tracing::info!(
                event = "stall.soft_pinged",
                number = task.number(),
                slot = naming::slot_key(slot),
                silence_min = silence / 60_000,
                "Мягкий пинг: ход завершён без транзакции"
            );
            return Ok(StallAct::SoftPinged);
        }
        tracing::info!(
            event = "stall.ping",
            number = task.number(),
            silence_min = silence / 60_000,
            "Сталл: автопинг зависшей стадии"
        );
        self.board
            .post_comment(
                task.number(),
                &format!(
                    "⏰ Автопинг сталл-детекта: сессия стадии `{}` молчит {} мин при статусе \
                     {:?} — ход, похоже, завершён без перевода статуса. Заверши транзакцию: \
                     переведи статус по канону пайпа (или продолжай работу и доведи её до \
                     статусного перевода).",
                    naming::slot_key(slot),
                    silence / 60_000,
                    task.status()
                ),
            )
            .await?;
        self.state
            .mark_stall_ping(&board.board, task.number(), task.status(), now);
        Ok(StallAct::Pinged)
    }

    fn log_action_failed(&self, task: &Task, e: UsecaseError) -> UsecaseError {
        tracing::error!(event = "action.failed", number = task.number(), error = %e, "Action failed");
        e
    }
}
