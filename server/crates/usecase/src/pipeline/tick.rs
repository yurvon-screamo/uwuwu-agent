//! Тик конвейера: чтение досок → домен решает → гейты → исполнение
//! через порты. Оркестрация знает слова (логи/промпты) — домен нет.

use super::actions::{LostAct, WakeAct};
use crate::error::{UsecaseError, UsecaseResult};
use crate::naming;
use crate::ports::{
    Clock, DecisionJudge, JudgeCfg, JudgeContext, JudgeSessionTail, Liveness, PrGateway,
    QuotaProbe, TaskBoardRepo, TickStateStore,
};
use crate::router::SessionRouter;
use crate::spawn_models::SpawnModels;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use uwuwu_domain::{
    decide, gate, spawn_guard, Action, Agent, Awaits, Gate, LoopOutcome, Priority, ReviewRoundStep,
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
    /// Семантических bounce автомержа (red CI / конфликт / PR не найден /
    /// closed / merge fail / отказ дозаполнения ячейки): исход «мерж не
    /// состоялся, причина на строке» — наблюдаемость исходов (#1519).
    /// Инкремент — единая точка в actions::bounce; pending/no-op не
    /// считаются. Транспортный Err пробы/мержа сюда не попадает: он
    /// пробрасывается из тика (\`tick.failed\`) и счётчиков не несёт.
    pub automerge_bounced: usize,
    /// Строк, переведённых в Needs attention спавн-петлёй.
    pub attention_flagged: usize,
    /// Автопингов сталл-детекта (ход завершён, статус не переведён).
    pub stall_pings: usize,
    /// Мягких пингов (idle-сессия, прямой промпт — санкция владельца 16.11).
    pub soft_pings: usize,
    /// Мягких пингов, отложенных CUP-капом (#1313): пинг — запуск хода,
    /// слот не выдан, mark не пишется — ретрай следующим тиком.
    pub soft_pings_deferred: usize,
    /// Строк, переведённых в Needs attention сталл-детектом.
    pub stall_flagged: usize,
    /// Строк, снятых с раунда капом ревью-раундов (#1311).
    pub review_flagged: usize,
    /// Вызовов судьи (#1364): точек сталла/капа, где вердикт запрошен.
    pub judge_calls: usize,
    /// Переносов строки вердиктом судьи (set_status, ADR-106 ревизия).
    pub judge_transitions: usize,
    /// Пробуждений вердиктом без переноса (вердикт = текущий статус).
    pub judge_wakeups: usize,
    /// Принудительных паркингов гвардом судьи (вызовы без прогресса).
    pub judge_forced: usize,
    /// Вердиктов, отклонённых гардом каноничности (hold-эквивалент).
    pub judge_rejected: usize,
    /// Строк, запаркованных порогом потерянных доставок (#1288).
    pub session_lost_flagged: usize,
    /// Отложенные политикой (пик/CUP) respawn'ы потерянных сессий (#1288):
    /// отдельно от будильников (\`wakeups_deferred\`).
    pub lost_deferred: usize,
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
    /// Строк на лимитной паузе (#1477): гейт классифицировал тишину как
    /// исчерпание окна подписки — пинги/эскалация не выполняются, сигнал
    /// массовости — tick_loop (main.rs).
    pub limit_paused: usize,
    /// Строк с недостоверной пробой активности исполнителя (Err): отказ
    /// сталл-прохода строки — без эскалации и записи каунтеров (#1281).
    pub probe_failed: usize,
    /// Имена executor'ов с транспортным отказом union-скана (#1312):
    /// громкость (порог/анти-повтор) и ТГ-сигнал — tick_loop (main.rs);
    /// тик несёт факт поимённо, дубль-детекции нет.
    pub executors_failed: Vec<String>,
    /// Доски плана, чей снапшот не прочитан (транспортный отказ после
    /// ретрая #1330): строки пропущены до следующего тика (#1503).
    /// Полный отказ (не прочитана ни одна) — Err наверх (tick.failed),
    /// сюда не попадает.
    pub boards_failed: Vec<String>,
}

/// Гейт CUP на доставке мягкого пинга (#1313): пинг — запуск хода,
/// слот выделяется как будильнику/спавну. Бандл «политика + занятость» —
/// один смысловой вход вердикт-прохода сталл-детекта; с #1626 — и
/// доставки раунд-промпта мастер-сессии (actions.rs).
#[derive(Debug, Clone, Copy)]
pub(crate) struct CupGate {
    pub(crate) policy: SpawnPolicy,
    pub(crate) active_now: usize,
}

impl CupGate {
    /// Кап полон — новый ход не выдаётся.
    pub(crate) fn full(&self) -> bool {
        self.active_now >= self.policy.cup
    }
}

/// Результат прохода сталл-детекта по строке.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StallAct {
    None,
    Pinged,
    /// Мягкий пинг: ход завершён (idle), прямой промпт в сессию.
    SoftPinged,
    /// Мягкий пинг отложен CUP-капом (#1313): пинг — запуск хода, слот
    /// не выдан, mark не пишется — доставка ретраится следующим тиком.
    Deferred,
    Flagged,
    /// Недостоверная проба исполнителя: сталл-проход строки отложен —
    /// вердикт о тишине не может быть получен (#1281, срез честности
    /// пробы). Эскалация и запись каунтеров не выполняются.
    ProbeFailed,
    /// Лимитная тишина (#1477): окно подписки исчерпано (проба квоты) —
    /// простой классифицирован как лимитный: пинги/каунтеры/судья/
    /// эскалация не выполняются, самовозобновление — пробой после спада.
    LimitPaused,
}

/// Полоса отказов пробы за тик, при которой тик сигнализирует деградацию
/// пробы одним громким событием (прецедент ограниченной деградации
/// ADR-086); обработка остальных строк не останавливается — изоляция
/// построчная (#1281).
pub const PROBE_DEGRADE_MIN_ROWS: usize = 5;

/// Порог потерянных доставок до парковки (#1288): тик ≈ 3–5 мин, три
/// неудачи подряд ≈ 9–15 мин; один сбой не паркуем (урок #1281).
pub const SESSION_LOST_ESCALATE_AFTER: u32 = 3;

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
    /// Судья — decision-модель статусных решений (#1364). None — контур
    /// выключен ([judge].enabled=false или нет ключа): тик живёт старой
    /// механикой; Err вызова — fallback на неё же (судья — улучшение,
    /// не зависимость).
    pub judge: Option<Arc<dyn DecisionJudge>>,
    /// Конфиг контура судьи (пороги/тримы/гвард — канон плана #1364).
    pub judge_cfg: JudgeCfg,
    /// Проба квоты подписки провайдера (#1477): гейт лимитной тишины —
    /// простой ходов из-за исчерпания окна не эскалируется. None — off
    /// ([quota] нет в конфиге); Err пробы — старая механика немедленно
    /// (проба улучшение, не зависимость).
    pub quota: Option<Arc<dyn QuotaProbe>>,
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
    /// Доска и слот жертвы: сброс луп-гварда/кулдауна и снятие ячейки
    /// на месте гашения (#1412).
    pub board: String,
    pub slot: Slot,
    pub number: u64,
    pub session: String,
    pub priority: Priority,
    /// ms спавна слота жертвы: детерминизм выбора (свежее — раньше).
    pub spawned_ms: u64,
}

/// Пул жертв тика: собирается в run() из union-скана, расходуется
/// HeldCup-ветками process_row (по одной жертве на отдачу слота).
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

/// Цель судьи на тике: строка+слот+сессия и факты сталл-точки. Свёртка
/// параметров (qlty function-parameters: judge_apply/judge_stall_action).
struct JudgeCall<'a> {
    board: &'a BoardPlan,
    task: &'a Task,
    slot: Slot,
    session: &'a str,
    /// Тишина stage-сессии (ms), вызовов без смены статуса и признак
    /// завершённого хода на момент решения (пробы гейта, не пересбор из
    /// хвостов — ревью L6: пустой сбор хвостов давал ложное «ход завершён»).
    silence: u64,
    calls: u32,
    idle: bool,
}

/// Char-safe обрезка строки до `max` символов (UTF-8: индексация байтами
/// рвёт многобайтовые символы — русский текст контекста судьи).
fn trim_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
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
            // Изоляция фейла доски (#1503): 502 одной доски не роняет тик —
            // её строки пропускаются до следующего прохода (poll 180 c),
            // транспортный ретрай нижнего уровня уже стоит (#1330).
            // Прецеденты деградации: enrich.failed (#1163), analyst
            // board_failed; полный отказ — гард ниже.
            let tasks = match self.board.read_board(&board.board).await {
                Ok(tasks) => tasks,
                Err(e) => {
                    out.boards_failed.push(board.board.clone());
                    tracing::warn!(
                        event = "board.read_failed",
                        board = %board.board,
                        error = %e,
                        "Снапшот доски не прочитан — строки пропущены до следующего тика"
                    );
                    continue;
                }
            };
            out.rows_total += tasks.len();
            boards.push((board, tasks));
        }
        // Гвард полного отказа (#1503): не прочитана ни одна доска плана —
        // tick.failed как раньше, изоляция не маскирует отказ «успешным»
        // пустым тиком. Ошибка называет весь состав упавших досок.
        if boards.is_empty() && !plan.boards.is_empty() {
            return Err(UsecaseError::Board(format!(
                "все доски плана не прочитаны: {}",
                out.boards_failed.join(", ")
            )));
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
        // CUP единый (#1231) с деградацией по исполнителю (#1450):
        // отказ одного исполнителя больше не обнуляет зрение конвейера.
        // Кап = реальный busy union-списка ЖИВЫХ исполнителей; отказной
        // вклад не даёт — его строки держит слепой hold (#1263), спавн
        // к нему невозможен, а занятые слоты мертвы вместе с ним.
        // Бывший strict-фолбэк («любой отказ → статусный счёт всех
        // досок») при одном офлайн-ноуте морозил весь конвейер: 37
        // активных строк при капе 7 = сутки spawns=0 (инцидент 21.11).
        // Гард спавна читает список СВОЕГО исполнителя (sight_by_executor,
        // #1263): живая сирота отказного исполнителя спрятана от дубль-
        // спавна слепотой его проектов, а не глобальным параличом.
        let per_exec = self.sessions.list_sessions_per_executor().await;
        // Контракт слияния — одна точка с роутером (router::merge_reachable,
        // ревью #1450): extend живых + имена отказных с warn по каждому.
        let (scan, executors_failed) = crate::router::merge_reachable(&per_exec);
        out.executors_failed = executors_failed;
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
        // Статусный фолбэк — только при ПОЛНОМ отказе всех исполнителей
        // (#1450): пустой union при живых исполнителях — это «слотов нет»,
        // а не «зрения нет».
        let any_ok = per_exec.iter().any(|(_, r)| r.is_ok());
        if !any_ok {
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
        // Сессии отказного исполнителя в скан не входят (#1450) — их
        // строки отсеются поиском: гасить без зрения нельзя (#1263);
        // idle-жертвы не попадают (капа не держат); high/medium не
        // гасятся никогда. Не-строки (кроны/боты/ручные) вне пула
        // структурно — отбор только по строкам досок.
        let preempt_pool = PreemptPool::default();
        for (bp, tasks) in boards.iter() {
            for t in tasks.iter() {
                if !t.status().is_active() {
                    continue;
                }
                let Some(slot) = t.stage_slot() else { continue };
                let Some(session) = t.session_of(slot) else {
                    continue;
                };
                let Some(info) = scan.iter().find(|s| s.id == *session) else {
                    continue; // мёртвая/невидимая — гасить нечего
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
                    board: bp.board.clone(),
                    slot,
                    number: t.number(),
                    session: session.to_string(),
                    priority: t.priority(),
                    spawned_ms,
                });
            }
        }
        let global_active: usize = if any_ok {
            scan.iter().filter(|s| s.busy).count()
        } else {
            fallback_active
        };
        out.sessions_busy = global_active;
        // Счётчик капа — один на весь тик, общий для всех досок (#1244):
        // спавн одной доски занимает слот глобального капа, следующая
        // доска видит занятость через ссылку
        let mut active_now = global_active;
        // Глобальная очередь кандидатов (#1586): строки всех досок — один
        // порядок до обхода. Ключ: тир is_continuation (классификация по
        // снапшоту до round_transition — см. doc в домене), приоритет,
        // доска в порядке конфига, порядок строк.
        // Дебаг-гейт одной строки (мок-Е2Е): UWUWU_ONLY_TASK=<number> —
        // конвейер действует только по ней, остальная доска read-only.
        let only = std::env::var("UWUWU_ONLY_TASK")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok());
        let now = self.clock.utc_minute();
        let now_ms = self.clock.now_ms();
        let mut rows: Vec<(usize, usize, &BoardPlan, Task)> = Vec::new();
        for (bi, (board, tasks)) in boards.iter_mut().enumerate() {
            for (ri, t) in tasks.drain(..).enumerate() {
                if only.is_some_and(|n| t.number() != n) {
                    continue;
                }
                rows.push((bi, ri, *board, t));
            }
        }
        rows.sort_by_key(|(bi, ri, _board, t)| {
            (
                !uwuwu_domain::is_continuation(t),
                t.priority().rank(),
                *bi,
                *ri,
            )
        });
        for (_, _, board, task) in &rows {
            // Пер-проектное зрение (#1263): контекст доски — список её
            // исполнителя; чужие исполнители зрение проекта не гасят.
            let executor = self.sessions.project_executor_name(&board.project);
            let sightings = sight_by_executor.get(executor).and_then(|v| v.as_deref());
            let ctx = RowContext {
                sightings,
                enrich_failed: &enrich_failed,
                preempts: &preempt_pool,
            };
            self.process_row(
                plan,
                board,
                task,
                now,
                now_ms,
                &mut active_now,
                &ctx,
                &mut out,
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
            lost_deferred = out.lost_deferred,
            automerged = out.automerged,
            automerge_bounced = out.automerge_bounced,
            held_live = out.held_live,
            stall_pings = out.stall_pings,
            soft_pings = out.soft_pings,
            soft_pings_deferred = out.soft_pings_deferred,
            stall_flagged = out.stall_flagged,
            limit_paused = out.limit_paused,
            review_flagged = out.review_flagged,
            session_lost_flagged = out.session_lost_flagged,
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

    /// Один ход обхода глобальной очереди кандидатов (#1586): строка уже
    /// заняла своё место в порядке; здесь — enrichment-гард, переход
    /// стадии, гард-цепочка и действие тика (спавн/автомерж/наблюдение).
    /// Эффекты — в общий счётчик капа и outcome тика; решение «какая
    /// строка следующая» здесь не принимается.
    #[allow(clippy::too_many_arguments)]
    async fn process_row(
        &self,
        plan: &TickPlan,
        board: &BoardPlan,
        task_ref: &Task,
        now: uwuwu_domain::UtcMinute,
        now_ms: u64,
        active_now: &mut usize,
        ctx: &RowContext<'_>,
        out: &mut TickOutcome,
    ) -> UsecaseResult<()> {
        // Отказ enrichment (#1163): строка пропускается — ячейки не
        // видны, decide по снапшоту даст вечный needs_spawn (механизм
        // дубль-спавнов ночи 15.11). Пропущенный тик ретраится даром.
        if ctx.enrich_failed.contains(&task_ref.number()) {
            out.enrich_skipped += 1;
            return Ok(());
        }
        let mut cleared_clone: Option<Task> = None;
        // Attention-механика: счётчики слотов, сохранённые при другом
        // статусе строки, устарели (строка ушла и вернулась) — сброс.
        // Live-only: observe стейт не пишет.
        if plan.mode == Mode::Live {
            self.state
                .reset_stale_spawn_loops(&board.board, task_ref.number(), task_ref.status());
            // Канон сессий: переход стадии = раунд слота. Мастер-сессии
            // ревьюверов (#1626) переживают циклы задачи в пределах стадии
            // (новый цикл — компакт + раунд-промпт в ту же сессию);
            // дев-семья едина между этапами и откатами (требование
            // владельца). Снимается только мёртвая ячейка — она не «едина»
            // ни с чем и замораживает кик владельца навсегда (#397); канон
            // fast-track: исключение от спавна — живая ячейка. Какой слот
            // обрабатывается — решает хелпер.
            let prev = self.state.last_seen_status(&board.board, task_ref.number());
            if prev.as_ref() != Some(&task_ref.status()) {
                cleared_clone = self.round_transition(board, task_ref, out).await;
                self.state
                    .set_last_seen_status(&board.board, task_ref.number(), task_ref.status());
                // Цикл-гвард ревью-контура (#1311): счёт подряд
                // request_changes по статус-переходам; кап достигнут —
                // эскалация вместо раунда N+1, строка снята с тика.
                if self.review_round_pass(board, prev, task_ref, out).await {
                    return Ok(());
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
                    return Ok(());
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
                                    "⚠︎ Spawn loop: the `{}` slot spawned >2 times without \
                                         status progress ({:?}). The row moved to \
                                         Needs attention — owner's triage.",
                                    naming::slot_key(slot),
                                    task.status()
                                ),
                            )
                            .await;
                    }
                    return Ok(());
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
                        return Ok(());
                    }
                    Some(SpawnHold::BlindScan) => {
                        out.held_blind += 1;
                        tracing::info!(
                                event = "spawn.held_blind_scan",
                                number = task.number(),
                                slot = naming::slot_key(slot),
                                "Spawn held: список исполнителя недоступен — спавн без зрения невозможен (#1263)"
                            );
                        return Ok(());
                    }
                    None => {}
                }
                let mut gated = gate(task, &action, plan.policy, *active_now, now);
                if matches!(gated, Gate::HeldCup { .. }) && plan.mode == Mode::Live {
                    // Отдача слота при понижении приоритета (#1259):
                    // спавн кандидата держит кап, есть busy-жертва
                    // строго ниже рангом → гасим её и отдаём слот на
                    // этом же тике (детали — в try_preempt).
                    if self.try_preempt(board, task, ctx, active_now, out).await {
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
                                        .set_session(&board.board, task.number(), slot, &session)
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
                                        // свежая сессия в ячейке — счётчик
                                        // прошлой сессии устарел (#1288)
                                        self.state.clear_lost_session(&board.board, task.number());
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
                            // тир — по снапшоту строки (task_ref): очередь
                            // строилась до round_transition, клон мог сменить
                            // фасетку (возврат в Design с мёртвой ячейкой)
                            tier = if uwuwu_domain::is_continuation(task_ref) {
                                "continuation"
                            } else {
                                "new"
                            },
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
                        .automerge(board, task, out)
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
                    self.observe_stage_session(board, plan, task, active_now, ctx, out)
                        .await;
                } else {
                    out.observed += 1;
                }
            }
        }
        Ok(())
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
                // Гашение — не смерть (#1412): слот отобран штатно, и
                // возврат жертвы не «спавн без прогресса». Сброс на месте
                // гашения: луп-гвард слота (иначе повторные гашения волной
                // дают 5 циклов → ложная «Респавн-петля»), кулдаун спавна
                // (возврат в очередь не ждёт 15 мин) и ячейка (мёртвый id
                // не остаётся — plain-путь подхватит строку по гейту).
                // Отказ записи на доску — краш-окно: строка остаётся со
                // старым id и деградирует к прежнему dead-respawn-пути.
                self.state
                    .clear_spawn_loop(&victim.board, victim.number, victim.slot);
                self.state
                    .clear_spawn_ms(&victim.board, victim.number, victim.slot);
                match self
                    .board
                    .clear_session(&victim.board, victim.number, victim.slot)
                    .await
                {
                    Ok(()) => {
                        tracing::info!(
                            event = "cup.preempt.cleared_cell",
                            victim = victim.number,
                            "Гашение — не смерть (#1412): ячейка снята, слот в очередь"
                        );
                    }
                    Err(e) => {
                        out.actions_failed += 1;
                        tracing::warn!(
                            event = "cup.preempt.clear_failed",
                            victim = victim.number,
                            error = %e,
                            "Гашение состоялось, снятие ячейки не удалось — строка обслужится dead-веткой (прежняя механика)"
                        );
                    }
                }
                tracing::info!(
                    event = "cup.preempt",
                    waiter = task.number(),
                    victim = victim.number,
                    victim_session = %victim.session,
                    "CUP: слот отдан — low-актив погашен (abort+delete), возобновление — возврат в очередь спавна с чистым счётчиком"
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
                // доставка прошла — серия потерянных доставок прервана (#1288)
                self.state.clear_lost_session(&board.board, task.number());
            }
            Ok(WakeAct::Respawned) => {
                out.spawns_done += 1;
                *active_now += 1;
                // новая сессия в ячейке — счётчик прошлой сессии устарел
                self.state.clear_lost_session(&board.board, task.number());
            }
            Ok(WakeAct::Deferred) => out.wakeups_deferred += 1,
            Ok(WakeAct::Guarded) => out.held_live += 1,
            Ok(WakeAct::GuardedBlind) => out.held_blind += 1,
            Ok(WakeAct::None) => match self
                .stall_pass(
                    board,
                    task,
                    CupGate {
                        policy: plan.policy,
                        active_now: *active_now,
                    },
                    out,
                )
                .await
            {
                Ok(StallAct::None) => {}
                Ok(StallAct::Pinged) => out.stall_pings += 1,
                Ok(StallAct::SoftPinged) => {
                    out.soft_pings += 1;
                    // доставленный ход занимает слот капа в этом же тике
                    // (#1231 семантика общих счётчиков: без инкремента K
                    // idle-строк масс-сталла видят один снапшот и оверселят кап)
                    *active_now += 1;
                    // мягкий пинг доставлен — доставка в сессию работает (#1288)
                    self.state.clear_lost_session(&board.board, task.number());
                }
                Ok(StallAct::Deferred) => out.soft_pings_deferred += 1,
                Ok(StallAct::Flagged) => out.stall_flagged += 1,
                Ok(StallAct::ProbeFailed) => out.probe_failed += 1,
                // лимитная пауза учтена гейтом (out.limit_paused), сигнал
                // массовости — tick_loop (main.rs)
                Ok(StallAct::LimitPaused) => {}
                // обе Err-ветки — путь доставки: классифицированная потеря
                // маршрутизируется, прочие отказы логируются как раньше (#1288)
                Err(e) => {
                    self.stage_delivery_err(board, plan, task, active_now, ctx, out, e)
                        .await;
                }
            },
            Err(e) => {
                self.stage_delivery_err(board, plan, task, active_now, ctx, out, e)
                    .await;
            }
        }
    }

    /// Единая Err-рука путей доставки стадии (#1288): SessionNotFound —
    /// dead-session класс, маршрутизация потери (`session_lost_pass`);
    /// прочие отказы — как раньше (лог, ретрай следующим тиком).
    #[allow(clippy::too_many_arguments)] // вердикт-проход: контекст строки+тики, прецедент stall_action
    async fn stage_delivery_err(
        &self,
        board: &BoardPlan,
        plan: &TickPlan,
        task: &Task,
        active_now: &mut usize,
        ctx: &RowContext<'_>,
        out: &mut TickOutcome,
        e: UsecaseError,
    ) {
        if !matches!(e, UsecaseError::SessionNotFound(_)) {
            out.actions_failed += 1;
            self.log_action_failed(task, e);
            return;
        }
        tracing::warn!(
            event = "delivery.session_lost",
            number = task.number(),
            error = %e,
            "Доставка в сессию стадии отклонена (dead-session) — маршрутизация потери"
        );
        match self
            .session_lost_pass(board, plan, task, *active_now, ctx)
            .await
        {
            Ok(LostAct::Respawned) => {
                out.spawns_done += 1;
                *active_now += 1;
            }
            Ok(LostAct::Escalated) => out.session_lost_flagged += 1,
            Ok(LostAct::Guarded) => out.held_live += 1,
            Ok(LostAct::GuardedBlind) => out.held_blind += 1,
            Ok(LostAct::Deferred) => out.lost_deferred += 1,
            // счётчик растёт внутри порога; либо луп-гвард сам эскалировал,
            // либо анти-ре-энтри отказ — исход учтён другими механизмами
            Ok(LostAct::Counted) => {}
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
        } else if prev == Some(Status::ReviewTesting) {
            // #1413: пара тест-ревью цикла — дайджест называет
            // последнюю сессию правильного слота
            Slot::TestReview
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
                // Контур судьи (#1364): автопаркинг капа заменяется вердиктом
                // decision-модели (она видит ленту — «цикл не сходится»).
                // Форс-паркинг без вызова: раунды CAP+escalate_after — гвард
                // сверху судьи, бесконечных «ещё раунд» нет. Err — fallback
                // на автопаркинг ниже. Цикл капа — стадия prev: тест-кап
                // (#1413, ReviewTesting→Testing) — fail-возвраты тест-
                // ревьювера, не request_changes; строка к этому тику уже в
                // Testing (в матрице судьи), поэтому гард — по prev.
                if self.judge_enabled() && prev.is_some_and(uwuwu_domain::judge_stage) {
                    return self
                        .judge_round_action(board, task, out, round_slot, next)
                        .await;
                }
                return self
                    .review_round_force_park(board, task, out, round_slot, next)
                    .await;
            }
            ReviewRoundStep::Reset => {
                self.state.set_review_rounds(&board.board, task.number(), 0);
                false
            }
            ReviewRoundStep::None => false,
        }
    }

    /// Смена статуса строки = раунд слота: решить судьбу ячейки.
    /// Сбой логируется в outcome. Возвращает очищенный клон строки.
    async fn round_transition(
        &self,
        board: &BoardPlan,
        task: &Task,
        out: &mut TickOutcome,
    ) -> Option<Task> {
        let slot = task.stage_slot()?;
        match self.transition_clear_slot(board, task, slot).await {
            Ok(cleared) => cleared,
            Err(e) => {
                out.actions_failed += 1;
                self.log_action_failed(task, e);
                None
            }
        }
    }

    /// Вход строки в стадию = раунд слота. Канон мастер-сессий (#1626,
    /// владелец 25.11.2026 дословно: «1 задача = 1 сессия на каждый этап
    /// ревью и между циклами мы их не меняем»): живая сессия ревьювера
    /// (PlanReview/ResultReview/TestReview) сохраняется между циклами
    /// задачи в пределах своей стадии — риск замыленности обыгран
    /// детерминированным компактом перед каждым новым циклом (п.2
    /// постановки: «автокомпактом этой сессии перед тем как делать к ней
    /// новый цикл»). Дев-семья (Design/Dev) — как раньше: живая едина
    /// между этапами и откатами (требование владельца). Снимаются:
    /// мёртвая ячейка (любого слота — она блокирует разворот кика
    /// навсегда, #397) и тест-слот (тестировщик — прежний канон свежей
    /// сессии на входе; мастер-сессии тестера — скоуп #1607); спавн/
    /// респавн несёт свежий промпт. Изоляция этапов и задач сохранена: план/код/
    /// тест-ревью — три разные сессии (п.1 постановки дословно), чужая
    /// задача — чужая строка. Unknown (serve недоступен) мёртвой не
    /// считается — консервативно, dead-детект wakeup-пути подхватит
    /// строку на следующем тике.
    /// Возвращает очищенный клон строки (тик работает с ним как с тенью).
    async fn transition_clear_slot(
        &self,
        board: &BoardPlan,
        task: &Task,
        slot: Slot,
    ) -> UsecaseResult<Option<Task>> {
        let Some(session) = task.session_of(slot) else {
            // первое вхождение в стадию: ячейки нет — спавн понесёт промпт
            return Ok(None);
        };
        // Канон мастер-сессий (#1626 + #1607): живая ячейка переживает
        // вход в стадию — снимается только мёртвая (она блокирует
        // разворот кика навсегда, #397). Ревьюверы — компакт + раунд-промпт
        // в неё же; тестер (#1607) — re-entry wake с дельта-промптом.
        let keep = self
            .sessions
            .project(&board.project)
            .liveness(session)
            .await
            != Liveness::Dead;
        if keep {
            if slot.is_review() {
                // Мастер-сессия ревьювера живёт: новый цикл = компакт +
                // раунд-промпт в неё же (срез 2 #1626). Доставка —
                // wakeup-путём (пик/CUP-гейты, ретрай, lost-маршрутизация):
                // pending-маркер в state.
                self.state
                    .set_round_reentry_pending(&board.board, task.number());
                tracing::info!(
                    event = "stage.reentry_keep",
                    number = task.number(),
                    slot = naming::slot_key(slot),
                    session = %session,
                    "Re-entry в ревью-стадию: сессия сохранена — компакт и раунд-промпт доставляются в неё же"
                );
            } else if slot == Slot::Test {
                // Мастер-сессия тестера живёт: повторный вход в Testing —
                // re-entry wake с дельта-промптом в неё же (#1607).
                // Доставка — wakeup-путём, тот же pending-маркер (#1626):
                // пустой хвост → re-entry wake, чужой хвост → wakeup.
                self.state
                    .set_round_reentry_pending(&board.board, task.number());
                tracing::info!(
                    event = "stage.reentry_keep",
                    number = task.number(),
                    slot = naming::slot_key(slot),
                    session = %session,
                    "Re-entry в Testing: сессия тестера сохранена — дельта-промпт доставляется в неё же"
                );
            }
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
            event = "stage.dead_cell_cleared",
            number = task.number(),
            slot = naming::slot_key(slot),
            session = %session,
            "Переход стадии: ячейка снята (мёртвая сессия) — спавн с новым промптом"
        );
        Ok(Some(cleared))
    }

    /// Сталл-детект: stage-сессия жива, но молчит дольше STALL_SILENCE_MS —
    /// ход завершён без статусного перевода (паттерн #1151/#1135/#384).
    /// Пинг агенту → повтор → Needs attention. Будит через механизм
    /// wakeup (пинг = коммент, следующий тик доставит его сессии).
    /// Гейт капа нужен доставке пинга: мягкий пинг — запуск хода,
    /// гейтуется как будильник (#1313).
    async fn stall_pass(
        &self,
        board: &BoardPlan,
        task: &Task,
        cup: CupGate,
        out: &mut TickOutcome,
    ) -> UsecaseResult<StallAct> {
        // без слота или сессии на слоте сталл не определён — тихий пропуск
        let Some((slot, session)) = task
            .stage_slot()
            .and_then(|slot| task.session_of(slot).map(|s| (slot, s.to_string())))
        else {
            return Ok(StallAct::None);
        };
        // Re-entry маркер открыт (#1607, #1626): вход ждёт доставки
        // wakeup-путём (раунд-промпт/дельта) — мягкий пинг «переведи
        // статус» сюда не ходит: инструкция ложна для держателя входа.
        if self
            .state
            .round_reentry_pending(&board.board, task.number())
        {
            tracing::debug!(
                event = "stall.reentry_open",
                number = task.number(),
                slot = naming::slot_key(slot),
                "Сталл-пасс пропущен: re-entry маркер открыт — вход доставляется wakeup-путём"
            );
            return Ok(StallAct::None);
        }
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
        let (gate, mut every, soft) = if idle {
            (STALL_SOFT_SILENCE_MS, STALL_SOFT_EVERY_MS, true)
        } else {
            (STALL_SILENCE_MS, STALL_PING_EVERY_MS, false)
        };
        if silence < gate {
            return Ok(StallAct::None);
        }
        // Судья (#1364): пороги тишины старой механики — её гейтинг,
        // интервал повторных решений — свой ([judge].every_ms; у soft-класса
        // старый интервал 30 мин держал бы повторный вердикт полчаса —
        // вскрыто e2e 14b: wake погасил шум, повторный вызов не наступал).
        // Override строго ПОД судьёй (по judge_stage): стадии вне полномочий
        // судьи (ReviewTesting #1413) сохраняют каденс старой механики —
        // override до статус-гарда замедлял бы эскалацию тест-ревью-зависов
        // вчетверо (2 ч вместо 30 мин; ревью раунд 4, Common-2).
        let judge_here = self.judge_enabled() && uwuwu_domain::judge_stage(task.status());
        if judge_here {
            every = self.judge_cfg.every_ms;
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
        // Раунд сталла переживает рестарт (#1585, ветка А — санкция
        // владельца 24.11: «Рестарт не должен приводить к потерям»; сброс
        // `round_stale` снесён): счётчик и метка пинга остаются
        // действительными — сессия та же (мастер-сессия #1626), тишина та
        // же. Инвариант #1281 сохранён: эскалация требует пинга,
        // доставленного ТЕКУЩИМ процессом — сталый раунд (последний пинг
        // до boot) не эскалирует и не зовёт судью; доставляется один
        // бутстрап-пинг, дальше контур идёт обычным ходом (см.
        // stall_action).
        let stale_round = count > 0 && last_ping < self.boot_ms;
        let now = self.clock.now_ms();
        if count > 0 && now.saturating_sub(last_ping) < every {
            return Ok(StallAct::None);
        }
        // Лимитный гейт (#1477): до пингов/судьи — см. quota_gate.
        if let Some(quota) = &self.quota {
            if let Some(act) = self.quota_gate(quota, task, out).await {
                return Ok(act);
            }
        }
        // Контур судьи (#1364): тот же гейтинг (тишина, раунд по статусу,
        // интервал повторов), решение — вердикт decision-модели вместо
        // пингов/автопаркинга. Err вызова — fallback на старую механику
        // ниже: судья — улучшение, не зависимость. Стадии вне матрицы
        // (ReviewTesting #1413 — вердикт тест-ревьювера) остаются на
        // пингах старой механики: вызов судьи там сжёг бы API — любой
        // вердикт гард отклоняет (judge_stage). Сталый раунд (#1585)
        // судье не отдаётся: гард ForceEscalate решает по каунтерам,
        // доставленным прошлым процессом — сначала бутстрап-пинг (#1281
        // инвариант), судья на следующих проходах.
        if judge_here && !stale_round {
            let call = JudgeCall {
                board,
                task,
                slot,
                session: &session,
                silence,
                calls: count,
                idle: soft,
            };
            return self.judge_stall_action(&call, soft, now, cup, out).await;
        }
        // Бутстрап-пинг сталого раунда (#1585 + инвариант #1281): раунд
        // пережил рестарт без сброса, но эскалация требует доставки в
        // этом процессе — один пинг, дальше контур идёт обычным ходом.
        if stale_round {
            return self
                .stall_ping_round(board, task, slot, &session, soft, silence, now, cup)
                .await;
        }
        self.stall_action(board, task, slot, &session, soft, silence, count, now, cup)
            .await
    }

    /// Лимитный гейт (#1477): исчерпание окна подписки — простой ходов
    /// нормативный, не сбой сессии. Каунтеры раунда не пишутся
    /// (пост-резетный пинг не задерживается), вердикт судьи на лимитной
    /// тишине не сжигается. Err пробы — None: старая механика немедленно,
    /// отказ пробы не парализует эскалацию сталл-детекта (проба —
    /// улучшение, не зависимость).
    async fn quota_gate(
        &self,
        quota: &Arc<dyn QuotaProbe>,
        task: &Task,
        out: &mut TickOutcome,
    ) -> Option<StallAct> {
        match quota.probe().await {
            Ok(snapshot) if snapshot.exhausted => {
                // пауза — норма контура (#1477): info, как stall.soft_pinged,
                // а не уровень эскалации; атрибуты — значениями, не Debug
                tracing::info!(
                    event = "stall.limit_paused",
                    number = task.number(),
                    window_pct = snapshot
                        .window_pct
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "none".into()),
                    reset_at_ms = snapshot
                        .reset_at_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "none".into()),
                    "Лимитная тишина: окно подписки исчерпано — строка на паузе \
                     без эскалации, продолжит после спада процента"
                );
                out.limit_paused += 1;
                Some(StallAct::LimitPaused)
            }
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(
                    event = "stall.quota_probe_err",
                    number = task.number(),
                    error = %e,
                    "Проба квоты недоступна — лимитный гейт пропущен, \
                     сталл-механика живёт как раньше"
                );
                None
            }
        }
    }

    /// Действие сталл-раунда (сталые раунды сюда не доходят — бутстрап
    /// выше, #1585/#1281): эскалация владельцу (пинги исчерпаны),
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
        cup: CupGate,
    ) -> UsecaseResult<StallAct> {
        if count < STALL_MAX_PINGS {
            return self
                .stall_ping_round(board, task, slot, session, soft, silence, now, cup)
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
                    "⚠︎ Stall detect: the `{}` stage session has been silent for {} min \
                     with the session alive — the agent is not completing the status \
                     transaction (executor probe: alive; auto-pings in the round: {count}). \
                     The row is in Needs attention, owner's triage.",
                    naming::slot_key(slot),
                    silence / 60_000
                ),
            )
            .await?;
        self.state
            .mark_stall_ping(&board.board, task.number(), task.status(), now);
        Ok(StallAct::Flagged)
    }

    // —— Контур судьи (#1364) ——

    /// Судья включён: порт есть и конфиг enabled (opt-in владельца).
    fn judge_enabled(&self) -> bool {
        self.judge_cfg.enabled && self.judge.is_some()
    }

    /// Судья на сталл-точке (#1364): замена пингов/автопаркинга. Гвард
    /// сверху — escalate_after вызовов без смены статуса строки →
    /// принудительный паркинг без нового вызова модели (слово владельца
    /// «порог 3 сверху судьи»; спайковый урок C7 — судья-«hold на всём»
    /// не глушит страховку). Err вердикта — fallback на старую механику.
    async fn judge_stall_action(
        &self,
        call: &JudgeCall<'_>,
        soft: bool,
        now: u64,
        cup: CupGate,
        out: &mut TickOutcome,
    ) -> UsecaseResult<StallAct> {
        if uwuwu_domain::judge_step(call.calls, self.judge_cfg.escalate_after)
            == uwuwu_domain::JudgeStep::ForceEscalate
        {
            return self
                .judge_park(
                    call,
                    &format!(
                        "⚖︎ Judge: guard — {} calls in a row without a status change \
                         (the {} threshold). The row is in Needs attention without \
                         a new model call. Owner's triage.",
                        call.calls, self.judge_cfg.escalate_after
                    ),
                    out,
                )
                .await;
        }
        let Some(judge) = self.judge.as_ref() else {
            // Инвариант judge_enabled() (гейт судьи выше) нарушен — порт
            // отсутствует при включённом контуре. Не глотаем действие
            // молча: деградация в старую механику сталл-детекта
            // (ревью раунд 4, Low).
            tracing::warn!(
                event = "judge.port_missing",
                number = call.task.number(),
                "Порт судьи отсутствует при enabled-гейте — fallback на старую механику"
            );
            return self
                .stall_action(
                    call.board,
                    call.task,
                    call.slot,
                    call.session,
                    soft,
                    call.silence,
                    call.calls,
                    now,
                    cup,
                )
                .await;
        };
        let ctx = self
            .judge_context(
                call.board,
                call.task,
                call.silence,
                call.calls,
                0,
                call.idle,
                false,
            )
            .await;
        match judge.decide(&ctx).await {
            Ok(verdict) => {
                out.judge_calls += 1;
                self.judge_apply(call, &verdict, now, out).await
            }
            Err(e) => {
                tracing::warn!(
                    event = "judge.err",
                    number = call.task.number(),
                    error = %e,
                    "Судья недоступна — fallback на старую механику сталл-детекта"
                );
                self.stall_action(
                    call.board,
                    call.task,
                    call.slot,
                    call.session,
                    soft,
                    call.silence,
                    call.calls,
                    now,
                    cup,
                )
                .await
            }
        }
    }

    /// Судья на кап-точке ревью-раундов (#1364): автопаркинг заменяется
    /// вердиктом (судья видит ленту — «цикл не сходится»). Два исхода:
    /// паркинг (вердикт Needs attention — дайджест с факторами вердикта)
    /// или «ещё раунд» (без мутаций — раунды считаются дальше, следующий
    /// cap-переход переспросит). Промптов сессиям здесь нет в любом
    /// исходе: следующий цикл доставляет раунд-промпт мастер-сессии
    /// (#1626), при паркинге строка снята с тика. Форс-паркинг без вызова:
    /// раунды CAP + escalate_after (escalate_after=0 — гвард выключен,
    /// семантика judge_step — ревью L4). Err — fallback на автопаркинг.
    /// true — строка снята с тика.
    async fn judge_round_action(
        &self,
        board: &BoardPlan,
        task: &Task,
        out: &mut TickOutcome,
        round_slot: Slot,
        rounds: u32,
    ) -> bool {
        let escalate_after = self.judge_cfg.escalate_after;
        if escalate_after > 0 && rounds >= REVIEW_ROUND_CAP + escalate_after {
            tracing::warn!(
                event = "judge.rounds_forced",
                number = task.number(),
                rounds = rounds,
                "Судья: гвард кап-точки — раунды за пределом, паркинг без вызова"
            );
            let last = task
                .session_of(round_slot)
                .map(str::to_string)
                .unwrap_or_else(|| "—".into());
            let digest = format!(
                "⚖︎ Judge: cap-point guard — {rounds} request_changes in a row \
                 (the {} cap + the {} threshold) — parking without a model call. \
                 The round is not assigned, the row is in Needs attention, \
                 owner's triage. The last review session: {last}.",
                REVIEW_ROUND_CAP, escalate_after
            );
            let parked = self.judge_round_park(board, task, &digest, out).await;
            if parked {
                out.judge_forced += 1;
            }
            return parked;
        }
        let Some(judge) = self.judge.as_ref() else {
            // Инвариант judge_enabled() (гейт кап-ветки выше) нарушен —
            // порт отсутствует при включённом контуре. Не глотаем решение
            // молча: деградация в старый автопаркинг кап-точки
            // (ревью раунд 4, Low).
            tracing::warn!(
                event = "judge.port_missing",
                number = task.number(),
                "Порт судьи отсутствует на кап-точке — fallback на автопаркинг"
            );
            return self
                .review_round_force_park(board, task, out, round_slot, rounds)
                .await;
        };
        let ctx = self
            .judge_context(board, task, 0, 0, rounds, false, true)
            .await;
        match judge.decide(&ctx).await {
            Ok(verdict) => {
                out.judge_calls += 1;
                if uwuwu_domain::verdict_target(task.status(), verdict.status)
                    != Some(Status::NeedsAttention)
                {
                    // «ещё раунд»: строка продолжает работу, мутаций и
                    // промптов нет (ревью-слот очищен до вызова — ревью C3)
                    tracing::info!(
                        event = "judge.rounds_continue",
                        number = task.number(),
                        rounds = rounds,
                        owner_needed = format!("{:.2}", verdict.owner_needed),
                        confidence = format!("{:.2}", verdict.confidence),
                        "Судья: цикл ещё сходится — раунд продолжается без паркинга"
                    );
                    return false;
                }
                // паркинг вердиктом: дайджест с факторами вердикта
                let last = task
                    .session_of(round_slot)
                    .map(str::to_string)
                    .unwrap_or_else(|| "—".into());
                let digest = format!(
                    "⚖︎ Judge: {rounds} request_changes in a row — the decision-model \
                     verdict: the loop does not converge (owner needed {:.2}, \
                     confidence {:.2}). The round is not assigned, the row is in \
                     Needs attention, owner's triage. The last review session: {last}.",
                    verdict.owner_needed, verdict.confidence
                );
                // инкременты после set_status (Low раунда 3): счётчики тика —
                // аудит судьи, отказ set_status не насчитывает паркинг,
                // которого не было
                let parked = self.judge_round_park(board, task, &digest, out).await;
                if parked {
                    out.review_flagged += 1;
                    out.judge_transitions += 1;
                }
                parked
            }
            Err(e) => {
                tracing::warn!(
                    event = "judge.err",
                    number = task.number(),
                    error = %e,
                    "Судья недоступна на кап-точке — fallback на автопаркинг"
                );
                self.review_round_force_park(board, task, out, round_slot, rounds)
                    .await
            }
        }
    }

    /// Хореография паркинга кап-точки — единственная копия (ревью раунда
    /// 2, L): race-чек → set_status → дайджест → сброс раундов. Race-чек —
    /// контур судьи (ревью раунд 4, Low): паркинг — тоже set_status,
    /// стейл-вердикт не перетирает свежую агентскую транзакцию; legacy-путь
    /// (judge off, автопаркинг) сохраняет прежний безусловный паркинг —
    /// инвариант «тик бит-в-бит при [judge] off». false — статус строки
    /// сменился во время вызова ИЛИ set_status отказал: счётчик стоит
    /// на капе, ретрай следующим count-переходом (раунд N+1 пройдёт —
    /// деградация benign).
    async fn judge_round_park(
        &self,
        board: &BoardPlan,
        task: &Task,
        digest: &str,
        out: &mut TickOutcome,
    ) -> bool {
        // race-чек (ревью раунд 4, Low): вердикт собирался секунды —
        // статус строки мог уехать. Отказ пробы трактуется как stale:
        // мутаций нет, ретрай следующим тиком
        if self.judge_enabled() {
            let fresh = match self.board.row_status(&board.board, task.number()).await {
                Ok(st) => st == Some(task.status()),
                Err(e) => {
                    tracing::warn!(
                        event = "judge.round_park_probe_err",
                        number = task.number(),
                        error = %e,
                        "Race-проба статуса перед кап-паркингом недостоверна — мутаций нет"
                    );
                    out.actions_failed += 1;
                    false
                }
            };
            if !fresh {
                tracing::info!(
                    event = "judge.round_park_stale",
                    number = task.number(),
                    "Кап-паркинг устарел — статус строки сменился во время вызова"
                );
                return false;
            }
        }
        if let Err(e) = self
            .board
            .set_status(&board.board, task.number(), Status::NeedsAttention)
            .await
        {
            out.actions_failed += 1;
            self.log_action_failed(task, e);
            return false;
        }
        if let Err(e) = self.board.post_comment(task.number(), digest).await {
            out.actions_failed += 1;
            self.log_action_failed(task, e);
        }
        self.state.set_review_rounds(&board.board, task.number(), 0);
        true
    }

    /// Автопаркинг кап-точки (старая механика; fallback судьи): кап
    /// достигнут — Needs attention + структурный дайджест, счётчик сброс.
    async fn review_round_force_park(
        &self,
        board: &BoardPlan,
        task: &Task,
        out: &mut TickOutcome,
        round_slot: Slot,
        next: u32,
    ) -> bool {
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
        let digest = format!(
            "⚠︎ Review-round cap: the {next}th request_changes in a row without an approve \
             (the {REVIEW_ROUND_CAP} threshold) — round {} is not assigned, the row is in \
             Needs attention, owner's triage. The findings and verdicts — in the thread \
             above; the last review session: {last}.",
            next + 1
        );
        let parked = self.judge_round_park(board, task, &digest, out).await;
        // инкремент после set_status (Low раунда 3, класс verdict-ветки):
        // отказ паркинга не насчитывает эскалацию, которой не было
        if parked {
            out.review_flagged += 1;
        }
        parked
    }

    /// Применение вердикта (сталл- и кап-точки; ревизия ADR-106 по санкции
    /// владельца 20.11 — сервер переносит статус по структурированному
    /// choice, не по текстам). Каноничность перехода — verdict_target;
    /// нелегальный вердикт — отклонён, hold-эквивалент (мутаций нет,
    /// счётчик гварда растёт). Вердикт = текущий статус — пробуждение
    /// агента без переноса. Race-чек (L2): статус строки сверяется перед
    /// set_status — вердикт не перетирает свежую агентскую транзакцию.
    async fn judge_apply(
        &self,
        call: &JudgeCall<'_>,
        verdict: &uwuwu_domain::JudgeVerdict,
        now: u64,
        out: &mut TickOutcome,
    ) -> UsecaseResult<StallAct> {
        let board = call.board;
        let task = call.task;
        let slot = call.slot;
        let session = call.session;
        let silence = call.silence;
        let calls = call.calls;
        let current = task.status();
        // вызов записан в носитель ДО применения: отклонённый/стейл вердикт —
        // тоже вызов без прогресса, гвард растёт от каждого (иначе модель с
        // устойчиво нелегальным choice перезапрашивается каждый тик до
        // исчерпания rpm-полосы — ревью C1); перенос статуса ниже сбрасывает
        // носитель clear_stall'ом
        self.state
            .mark_stall_ping(&board.board, task.number(), current, now);
        let Some(target) = uwuwu_domain::verdict_target(current, verdict.status) else {
            out.judge_rejected += 1;
            tracing::warn!(
                event = "judge.rejected",
                number = task.number(),
                verdict = verdict.status.key(),
                current = ?current,
                "Вердикт судьи отклонён гардом каноничности — hold-эквивалент"
            );
            return Ok(StallAct::None);
        };
        if target == current {
            // пробуждение без переноса — функциональный наследник мягкого
            // пинга: судья считает, что агент должен продолжить
            self.judge_prompt(
                board,
                slot,
                session,
                &format!(
                    "⚖︎ Pipeline judge: the row has been silent for {} min — keep \
                     working. Finish the turn with a status transaction \
                     (uwuwu_task_edit).",
                    silence / 60_000
                ),
            )
            .await?;
            out.judge_wakeups += 1;
            tracing::info!(
                event = "judge.wake",
                number = task.number(),
                owner_needed = format!("{:.2}", verdict.owner_needed),
                confidence = format!("{:.2}", verdict.confidence),
                "Судья: пробуждение агента без переноса"
            );
            return Ok(StallAct::None);
        }
        // race-чек (ревью L2): вердикт собирался секунды — статус строки
        // мог уехать агентской транзакцией; не перетираем свежий переход
        if self.board.row_status(&board.board, task.number()).await? != Some(current) {
            tracing::info!(
                event = "judge.stale",
                number = task.number(),
                "Вердикт судьи устарел — статус строки сменился во время вызова"
            );
            return Ok(StallAct::None);
        }
        self.board
            .set_status(&board.board, task.number(), target)
            .await?;
        let digest = format!(
            "⚖︎ Judge: the row moved to {} (silence {} min; owner needed: \
             {:.2}; confidence {:.2}; calls without progress: {}). The reason — \
             the decision-model verdict on the task context (body, comment \
             thread, session tails). ADR-106 revision #1364.",
            naming::status_name(target),
            silence / 60_000,
            verdict.owner_needed,
            verdict.confidence,
            calls + 1
        );
        if let Err(e) = self.board.post_comment(task.number(), &digest).await {
            // перенос состоялся; коммент — наблюдаемость, не критичный путь
            out.actions_failed += 1;
            self.log_action_failed(task, e);
        }
        // Промпт старой сессии НЕ отправляется (ревью C2): адресат — агент
        // прошлой стадии, контракту новой стадии он не отвечает; «перенос =
        // пинг» реализуется самим переносом — агент новой стадии спавнится
        // штатной механикой следующего тика. Промпт осиротевшей сессии —
        // сожжённый LLM-раунд не на том агенте.
        self.state.clear_stall(&board.board, task.number());
        out.judge_transitions += 1;
        tracing::info!(
            event = "judge.moved",
            number = task.number(),
            target = ?target,
            owner_needed = format!("{:.2}", verdict.owner_needed),
            confidence = format!("{:.2}", verdict.confidence),
            "Судья: перенос строки вердиктом"
        );
        if target == Status::NeedsAttention {
            return Ok(StallAct::Flagged);
        }
        Ok(StallAct::None)
    }

    /// Принудительный паркинг гварда судьи (без вызова модели).
    async fn judge_park(
        &self,
        call: &JudgeCall<'_>,
        digest: &str,
        out: &mut TickOutcome,
    ) -> UsecaseResult<StallAct> {
        let board = call.board;
        let task = call.task;
        let session = call.session;
        tracing::warn!(
            event = "judge.forced",
            number = task.number(),
            "Судья: гвард вызовов без прогресса — принудительный паркинг"
        );
        // race-чек (ревью раунд 4, Low): паркинг — тоже set_status,
        // гвард не перетирает свежую агентскую транзакцию; stale —
        // строка ушла сама, гвард-счётчик живёт дальше по носителю
        if self.board.row_status(&board.board, task.number()).await? != Some(task.status()) {
            tracing::info!(
                event = "judge.park_stale",
                number = task.number(),
                "Принудительный паркинг устарел — статус строки сменился во время тика"
            );
            return Ok(StallAct::None);
        }
        self.board
            .set_status(&board.board, task.number(), Status::NeedsAttention)
            .await?;
        if let Err(e) = self.board.post_comment(task.number(), digest).await {
            out.actions_failed += 1;
            self.log_action_failed(task, e);
        }
        // будим агента: строка покидает активную стадию
        self.judge_prompt(
            board,
            task.stage_slot().unwrap_or(Slot::Dev),
            session,
            "⚖︎ Pipeline judge: the row moved to Needs attention — the turn is \
             finished, work is paused until owner's triage.",
        )
        .await?;
        self.state.clear_stall(&board.board, task.number());
        out.judge_forced += 1;
        Ok(StallAct::Flagged)
    }

    /// Промпт агенту слота от судьи: пин агента слота (#1300) + прямой
    /// промпт — механика мягкого пинга. Ошибка доставки — Err вызывающему
    /// (тик переживёт: вердикт применён до промпта).
    async fn judge_prompt(
        &self,
        board: &BoardPlan,
        slot: Slot,
        session: &str,
        text: &str,
    ) -> UsecaseResult<()> {
        let gw = self.sessions.project(&board.project);
        if let Some(agent) = Agent::of_slot(slot) {
            gw.switch_agent(session, agent).await?;
        }
        gw.prompt(session, text).await
    }

    /// Сборка JudgeContext (#1364): тело, хвост ленты, серверные факты,
    /// хвосты сессий слотов. Отказы чтений — не блокируют вердикт:
    /// контекст неполон, warn-лог (судья решает по тому, что есть).
    #[allow(clippy::too_many_arguments)]
    async fn judge_context(
        &self,
        board: &BoardPlan,
        task: &Task,
        silence: u64,
        calls: u32,
        rounds: u32,
        idle: bool,
        cap_point: bool,
    ) -> JudgeContext {
        let cfg = self.judge_cfg.clone();
        let body = match self.board.read_issue(&board.board, task.number()).await {
            Ok(view) => trim_chars(&view.body, cfg.body_max_chars),
            Err(e) => {
                tracing::warn!(
                    event = "judge.ctx_body_err",
                    number = task.number(),
                    error = %e,
                    "Тело задачи в контекст судьи не попало"
                );
                String::new()
            }
        };
        let comments: Vec<(u64, String)> = match self.board.latest_comments(task.number()).await {
            Ok(list) => {
                let mut v: Vec<(u64, String)> = list
                    .into_iter()
                    .map(|c| (c.created_ms, trim_chars(&c.body, cfg.comment_max_chars)))
                    .collect();
                v.sort_by_key(|(ms, _)| std::cmp::Reverse(*ms));
                v.truncate(cfg.comments_tail);
                v
            }
            Err(e) => {
                tracing::warn!(
                    event = "judge.ctx_comments_err",
                    number = task.number(),
                    error = %e,
                    "Лента комментов в контекст судьи не попала"
                );
                Vec::new()
            }
        };
        let mut sessions = Vec::new();
        for slot in [
            Slot::Design,
            Slot::Dev,
            Slot::PlanReview,
            Slot::ResultReview,
            Slot::Test,
        ] {
            let Some(session) = task.session_of(slot) else {
                continue;
            };
            match self
                .sessions
                .project(&board.project)
                .last_assistant_reply(session)
                .await
            {
                Ok(Some(reply)) => sessions.push(JudgeSessionTail {
                    slot,
                    text: trim_chars(&reply.text, cfg.msg_max_chars),
                    finished: reply.finished,
                }),
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        event = "judge.ctx_session_err",
                        number = task.number(),
                        slot = naming::slot_key(slot),
                        error = %e,
                        "Хвост сессии в контекст судьи не попал"
                    );
                }
            }
        }
        let now = self.clock.now_ms();
        let restart_recent_min = (now.saturating_sub(self.boot_ms) < 60 * 60_000)
            .then_some((now.saturating_sub(self.boot_ms)) / 60_000);
        JudgeContext {
            board: board.board.clone(),
            number: task.number(),
            title: task.title().to_string(),
            status: task.status(),
            slot: task.stage_slot().unwrap_or(Slot::Dev),
            silence_min: silence / 60_000,
            idle,
            judge_calls_no_progress: calls,
            review_rounds: rounds,
            branch: task.branch().map(|b| b.as_str().to_string()),
            pr_url: task.pr().map(|p| p.as_str().to_string()),
            restart_recent_min,
            body,
            comments,
            sessions,
            now_ms: now,
            cap_point,
        }
    }

    /// Один раунд пинга: мягкий — прямым промптом в сессию (мгновенное
    /// пробуждение, без обхода через коммент), классический — комментом.
    /// Мягкий пинг — запуск хода: при полном капе держится без mark
    /// (доставка ретраится следующим тиком), классический — коммент без
    /// гейта (ход стартует только гейтованным будильником) (#1313).
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
        cup: CupGate,
    ) -> UsecaseResult<StallAct> {
        if soft {
            if cup.full() {
                tracing::info!(
                    event = "stall.soft_ping_held_cup",
                    number = task.number(),
                    active = cup.active_now,
                    cap = cup.policy.cup,
                    // мягкий пинг достижим только при сессии на слоте
                    // стадии — тир продолжений всегда
                    tier = "continuation",
                    "Мягкий пинг отложен: CUP cap — запуск хода ждёт слот"
                );
                return Ok(StallAct::Deferred);
            }
            let gw = self.sessions.project(&board.project);
            // пин агента слота перед ходом (#1300) — как на будильнике
            if let Some(agent) = Agent::of_slot(slot) {
                gw.switch_agent(session, agent).await?;
            }
            gw.prompt(
                session,
                &format!(
                    "⏰ Soft ping: the turn has finished without the status transition — \
                     row #{} is stalled. Close the transaction per the turn contract: \
                     uwuwu_task_edit {{number: {}, status: <next stage>}} \
                     or ask the owner in a comment + Needs attention.",
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
                    "⏰ Stall auto-ping: the `{}` stage session has been silent for {} min at status \
                     {:?} — the turn seems to have finished without the status transition. Close \
                     the transaction: set the status per the pipeline canon (or keep working \
                     until the status transition is done).",
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

    pub(crate) fn log_action_failed(&self, task: &Task, e: UsecaseError) -> UsecaseError {
        tracing::error!(event = "action.failed", number = task.number(), error = %e, "Action failed");
        e
    }
}
