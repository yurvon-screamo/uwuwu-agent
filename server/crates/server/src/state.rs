//! Персистентное состояние тика: JSON-файл (volume), атомарная запись.
//! Реализация порта TickStateStore.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use uwuwu_domain::{Slot, Status};
use uwuwu_usecase as naming;
use uwuwu_usecase::BotStateStore;
use uwuwu_usecase::Clock;
use uwuwu_usecase::CronRow;
use uwuwu_usecase::CronRunTrack;
use uwuwu_usecase::CronStateStore;
use uwuwu_usecase::DreamerMark;
use uwuwu_usecase::TickStateStore;
use uwuwu_usecase::{UsecaseError, UsecaseResult};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RowState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_comment_id: Option<String>,
    /// Последний статус, виденный тиком (детект переходов стадии —
    /// канон сессий: переход стадии = раунд слота; мастер-сессии
    /// ревьюверов переживают циклы, #1626).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_status: Option<String>,
    /// Сталл-детект: (статус раунда, ms пинга, счётчик) — смена статуса
    /// обнуляется записью нового статуса (см. mark_stall_ping).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stall: Option<(String, u64, u32)>,
    /// Потерянные доставки (#1288): (статус раунда, счётчик подряд) —
    /// смена статуса = новый раунд; успех доставки сбрасывает.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lost: Option<(String, u32)>,
    /// Pending раунд-промпт мастер-сессии ревьювера (#1626): re-entry в
    /// ревью-стадию с живой сессией ждёт доставки (компакт + промпт).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    round_reentry: bool,
}

/// Состояние бота (секция `bots`): вечная сессия + offset апдейтов ТГ.
/// Сессия переживает stop/re-arm; пересоздание — clear session, offset
/// остаётся (поллинг продолжается с непрочитанного).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct BotRow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    offset: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    manual_hash: Option<u64>,
    /// Hash уроков, доставленных префиксом (agent-режим #1424): None —
    /// не доставлены/сброс (компакт/recreate); персиста переживает рестарт.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lessons_hash: Option<u64>,
    /// Метка последней адмиссии нативного компакта (ms; None — не было):
    /// коулдаун автокомпакта переживает рестарт сервера (#1221, ADR-082).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    compact_ms: Option<u64>,
}

/// Мастер-привязка роли проекта (#1634, форма роль x проект общая
/// с #1607): вечная сессия волны + модель рождения + checkout.
/// Пересоздание - перезапись записи, старая сессия не гасится.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MasterRow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkout: Option<String>,
    /// Метка последней адмиссии компакта (ms; None - не было):
    /// коулдаун переживает рестарт сервера.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    compact_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StateFile {
    /// «board:number» → состояние строки.
    #[serde(default)]
    rows: BTreeMap<String, RowState>,
    /// Метки дример-обработки: id сессии → watermark/вечный скип (#1306).
    #[serde(default)]
    dreamer: BTreeMap<String, DreamerMark>,
    /// «исходная → дример-сессия» — инвариант 1:1 ADR-005 (#1306).
    #[serde(default)]
    dreamer_links: BTreeMap<String, String>,
    /// Legacy вечных маркеров (эпоха до #1306): читается со старого ключа,
    /// мигрирует при загрузке в `UpTo(now)`, в новом файле не пишется.
    #[serde(
        default,
        rename = "dreamer_done",
        skip_serializing_if = "Vec::is_empty"
    )]
    legacy_dreamer_done: Vec<String>,
    /// «board:number:slot» → ms последнего спавна (анти-шторм).
    #[serde(default)]
    spawns: BTreeMap<String, u64>,
    /// «board:number:slot» → (статус на прошлом спавне, повторов) —
    /// attention-механика.
    #[serde(default)]
    spawn_loops: BTreeMap<String, (String, u32)>,
    /// «board:number» → подряд request_changes ревью-контура без approve
    /// (#1311): персистентный счётчик, смены статуса внутри цикла не
    /// сбрасывают.
    #[serde(default)]
    review_rounds: BTreeMap<String, u32>,
    /// «board:number» → (статус цикла, кругов) — цикл-гвард аналитики (#1135).
    #[serde(default)]
    analys_loops: BTreeMap<String, (String, u32)>,
    /// Дневной счётчик дримера: (дата JST, спавнов).
    #[serde(default)]
    dreamer_day: Option<(String, u32)>,
    /// «slug бота» → сессия + offset (раннер ботов, #1150).
    #[serde(default)]
    bots: BTreeMap<String, BotRow>,
    /// «имя крона» → машинный учёт ранов (#1480, снос журнала):
    /// last_fire — источник is_due, run — детект тишины UC-02,
    /// deferred_since — детект пропуска файра, fails — счётчик отказов
    /// (канон ADR-103 «runtime-факты — в state»).
    #[serde(default)]
    crons: BTreeMap<String, CronRowState>,
    /// Реестр серверных постов: node_id → ms поста (self-эхо-фильтр #1204).
    /// Id GitHub глобально уникальны — плоский map без ключа доски.
    #[serde(default)]
    own_comments: BTreeMap<String, u64>,
    /// Мастер-привязки волн (#1634, форма роль:проект общая с #1607).
    #[serde(default)]
    master: BTreeMap<String, MasterRow>,
}

/// Машинный учёт крона на носителе (срез #1480): SERDE-форма CronRow
/// (usecase::ports); трек без сессии = краш между записью и спавном.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CronRowState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_fire_ms: Option<u64>,
    /// Якорь первой встречи (#1517): None не файрит — крон без факта
    /// ждёт ближайшей плановой минуты. serde default — старые state
    /// (без поля) читаются.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    anchor_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deferred_since_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run: Option<CronRunState>,
    #[serde(default)]
    fails: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CronRunState {
    session: String,
    fired_ms: u64,
}

impl From<&CronRow> for CronRowState {
    fn from(row: &CronRow) -> Self {
        Self {
            last_fire_ms: row.last_fire_ms,
            anchor_ms: row.anchor_ms,
            deferred_since_ms: row.deferred_since_ms,
            run: row.run.as_ref().map(|r| CronRunState {
                session: r.session.clone(),
                fired_ms: r.fired_ms,
            }),
            fails: row.fails,
        }
    }
}

impl From<CronRowState> for CronRow {
    fn from(s: CronRowState) -> Self {
        CronRow {
            last_fire_ms: s.last_fire_ms,
            anchor_ms: s.anchor_ms,
            deferred_since_ms: s.deferred_since_ms,
            run: s.run.map(|r| CronRunTrack {
                session: r.session,
                fired_ms: r.fired_ms,
            }),
            fails: s.fails,
        }
    }
}

/// Переезд носителя ботов (#1214): ключи старой раскладки
/// `Projects/<proj>/Боты/<имя>` → `bot/<имя>`. Существующий новый
/// ключ приоритетен — свежие записи не перетираются; коллизия двух
/// легаси-ключей с одинаковым именем бота — громкий warn (тихий дроп
/// строки состояния прячет потерю сессии/offset).
fn migrate_bot_keys(bots: &mut BTreeMap<String, BotRow>) {
    let legacy: Vec<String> = bots
        .keys()
        .filter(|k| {
            let segs: Vec<&str> = k.split('/').collect();
            segs.len() == 4 && segs[0] == "Projects" && segs[2] == "Боты"
        })
        .cloned()
        .collect();
    for key in legacy {
        let Some(row) = bots.remove(&key) else {
            continue;
        };
        let name = key.rsplit('/').next().unwrap_or_default();
        let new = format!("bot/{name}");
        if bots.contains_key(&new) {
            tracing::warn!(event = "state.bot_key_collision", from = %key, to = %new,
                "Коллизия легаси-ключей ботов — запись старого ключа отброшена (новый приоритетен)");
            continue;
        }
        bots.insert(new, row);
    }
}

pub struct JsonStateStore {
    path: PathBuf,
    state: std::sync::Mutex<StateFile>,
    /// Сериализация записи файла: конкурентные save (тик-сайдкары ×
    /// per-bot раннеры) писали общий tmp с truncate — интерливинг двух
    /// записей портил state.json, следующий старт брал тихий сброс
    /// (класс масс-догона кронов, #1517).
    save_lock: std::sync::Mutex<()>,
    /// Штамп времени возрастного GC реестра серверных постов.
    clock: Arc<dyn Clock>,
}

impl JsonStateStore {
    pub fn load(path: PathBuf, clock: Arc<dyn Clock>) -> Self {
        // Чтение в bytes: невалидный UTF-8 доходит до парсера и ловится
        // как коррупция (громко + улика), а не тихим InvalidData чтения.
        let mut state: StateFile = match std::fs::read(&path) {
            // Файла нет — первый старт: тихо.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => StateFile::default(),
            // Иная ошибка чтения (права, сбой тома) — громко: тихий сброс
            // учёта кронов невидим оператору (#1517). Улика — попыткой
            // rename: ему нужны права на каталог, не на чтение файла.
            Err(e) => {
                tracing::error!(
                    event = "state.load_unreadable",
                    path = %path.display(),
                    error = %e,
                    "state.json не читается — старт с пустым state, улика сохраняется rename'ом (сброс учёта кронов: окна тишины UC-02 обнулены, файры возобновятся с ближайших плановых минут)"
                );
                preserve_corrupt_state(&path, clock.now_ms());
                StateFile::default()
            }
            Ok(raw) => match serde_json::from_slice(&raw) {
                Ok(state) => state,
                // Битый файл — громко: класс инцидента #1517 (тихий сброс
                // раньше давал масс-догон, ныне — якоря и тишину до
                // плановых минут; оператору важно знать о сбросе учёта).
                // Улика сохраняется рядом (одна последняя), старт
                // продолжается.
                Err(e) => {
                    tracing::error!(
                        event = "state.load_corrupt",
                        path = %path.display(),
                        error = %e,
                        "state.json не парсится — старт с пустым state, битый файл сохранён рядом (сброс учёта кронов: окна тишины UC-02 обнулены, файры возобновятся с ближайших плановых минут)"
                    );
                    preserve_corrupt_state(&path, clock.now_ms());
                    StateFile::default()
                }
            },
        };
        // Миграция ключей ботов при переезде носителя (#1214): старая
        // раскладка `Projects/<proj>/Боты/<имя>` → `bot/<имя>`. Сессия
        // и offset переживают переезд; существующий новый ключ
        // приоритетен (не перетираем свежие записи).
        migrate_bot_keys(&mut state.bots);
        // Миграция legacy вечных маркеров (#1306) с разбором когорт:
        // в старом векторе лежали и рабочие сессии, и бот-сессии (старый
        // раннер маркировал их как вечный скип). Бот-когорта (id из
        // реестра `bots[*].session`) сохраняет Forever — иначе живой бот
        // после деплоя становится перманентным кандидатом дримера
        // (сообщение → idle → дрим → снова, ревью #1306). Рабочие
        // сессии — UpTo(момент загрузки): revival'ы после деплоя
        // дообрабатываются, старые сессии молчат (шторм исключён).
        if !state.legacy_dreamer_done.is_empty() {
            let boot = clock.now_ms();
            let bot_sessions: std::collections::HashSet<&String> = state
                .bots
                .values()
                .filter_map(|bot| bot.session.as_ref())
                .collect();
            for id in state.legacy_dreamer_done.drain(..) {
                let mark = if bot_sessions.contains(&id) {
                    DreamerMark::Forever
                } else {
                    DreamerMark::UpTo(boot)
                };
                state.dreamer.entry(id).or_insert(mark);
            }
        }
        Self {
            path,
            state: std::sync::Mutex::new(state),
            save_lock: std::sync::Mutex::default(),
            clock,
        }
    }

    fn key(board: &str, number: u64) -> String {
        format!("{board}:{number}")
    }

    /// Атомарная запись файла (единая точка: оба трейта — делегаты).
    /// save_lock захватывается до снапшота: сериализация и запись под
    /// одним замком — порядок файлов = порядок вызовов save (иначе
    /// писатель со старым снапшотом, захватив лок позже, перезаписал бы
    /// более свежий файл).
    fn save_now(&self) -> UsecaseResult<()> {
        let _writer = self.save_lock.lock().expect("save lock");
        {
            let mut guard = self.state.lock().expect("state lock");
            if guard.spawns.len() > 5000 {
                let keys: Vec<String> = guard.spawns.keys().cloned().collect();
                let cut = guard.spawns.len() - 5000;
                for k in keys.into_iter().take(cut) {
                    guard.spawns.remove(&k);
                }
            }
            if guard.spawn_loops.len() > 5000 {
                let keys: Vec<String> = guard.spawn_loops.keys().cloned().collect();
                let cut = guard.spawn_loops.len() - 5000;
                for k in keys.into_iter().take(cut) {
                    guard.spawn_loops.remove(&k);
                }
            }
            if guard.review_rounds.len() > 5000 {
                let keys: Vec<String> = guard.review_rounds.keys().cloned().collect();
                let cut = guard.review_rounds.len() - 5000;
                for k in keys.into_iter().take(cut) {
                    guard.review_rounds.remove(&k);
                }
            }
            // legacy-ключи триажа (эпоха до #1135) — вечный мёртвый груз
            guard.spawns.retain(|k, _| !k.ends_with(":triage"));
            // Реестр серверных постов (#1204): записи старше недели мертвы
            // (журнал комментов задачи держит последнюю сотню) — возрастной
            // GC; кап — страховка от аномального роста.
            let week_ago = self.clock.now_ms().saturating_sub(7 * 24 * 60 * 60 * 1000);
            guard.own_comments.retain(|_, posted| *posted >= week_ago);
            if guard.own_comments.len() > 5000 {
                let keys: Vec<String> = guard.own_comments.keys().cloned().collect();
                let cut = guard.own_comments.len() - 5000;
                for k in keys.into_iter().take(cut) {
                    guard.own_comments.remove(&k);
                }
            }
        }
        let raw = {
            let guard = self.state.lock().expect("state lock");
            serde_json::to_vec_pretty(&*guard)
        };
        let raw = raw.map_err(|e| UsecaseError::State(format!("сериализация: {e}")))?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| UsecaseError::State(format!("mkdir {}: {e}", dir.display())))?;
        }
        // fsync tmp перед rename + fsync каталога после: без сброса
        // каталога rename может не дожить до диска при падении хоста
        // (процессный рестарт безопасен — содержимое tmp уже на диске).
        // Битый state на следующем старте — тихий сброс учёта кронов
        // (класс масс-догона, #1517).
        let tmp = self.path.with_extension("tmp");
        let mut file = std::fs::File::create(&tmp)
            .map_err(|e| UsecaseError::State(format!("create {}: {e}", tmp.display())))?;
        file.write_all(&raw)
            .map_err(|e| UsecaseError::State(format!("write {}: {e}", tmp.display())))?;
        file.sync_all()
            .map_err(|e| UsecaseError::State(format!("sync {}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| UsecaseError::State(format!("rename {}: {e}", self.path.display())))?;
        if let Some(dir) = self.path.parent() {
            if let Ok(d) = std::fs::File::open(dir) {
                let _ = d.sync_all();
            }
        }
        Ok(())
    }
}

/// Улика битого state: переименовать в `<имя>.corrupt-<unix сек>`,
/// предыдущие копии снести (удержание одной последней). Отказ сохранения
/// — warn, старт продолжается (громкий error уже отдан).
fn preserve_corrupt_state(path: &std::path::Path, now_ms: u64) {
    let base = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("state.json")
        .to_string();
    let prefix = format!("{base}.corrupt-");
    let backup = path.with_file_name(format!("{prefix}{}", now_ms / 1000));
    if let Some(dir) = path.parent() {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with(&prefix) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
    if let Err(e) = std::fs::rename(path, &backup) {
        tracing::warn!(
            event = "state.corrupt_preserve_failed",
            error = %e,
            "Битый state.json не сохранён как улика (старт продолжен)"
        );
    }
}

impl TickStateStore for JsonStateStore {
    fn last_comment_id(&self, board: &str, number: u64) -> Option<String> {
        let guard = self.state.lock().expect("state lock");
        guard
            .rows
            .get(&Self::key(board, number))
            .and_then(|r| r.last_comment_id.clone())
    }

    fn last_seen_status(&self, board: &str, number: u64) -> Option<Status> {
        let guard = self.state.lock().expect("state lock");
        guard
            .rows
            .get(&Self::key(board, number))
            .and_then(|r| r.last_status.as_deref())
            .map(naming::parse_status)
    }

    fn set_last_seen_status(&self, board: &str, number: u64, status: Status) {
        let mut guard = self.state.lock().expect("state lock");
        let name = naming::status_name(status).to_string();
        guard
            .rows
            .entry(Self::key(board, number))
            .or_default()
            .last_status = Some(name);
    }

    fn stall_ping(&self, board: &str, number: u64) -> Option<(uwuwu_domain::Status, u64, u32)> {
        let guard = self.state.lock().expect("state lock");
        guard
            .rows
            .get(&Self::key(board, number))
            .and_then(|r| r.stall.as_ref())
            .map(|(st, ms, c)| (naming::parse_status(st), *ms, *c))
    }

    fn mark_stall_ping(&self, board: &str, number: u64, status: uwuwu_domain::Status, at_ms: u64) {
        let mut guard = self.state.lock().expect("state lock");
        let row = guard.rows.entry(Self::key(board, number)).or_default();
        // счётчик растёт внутри раунда (одинаковый статус); смена статуса = 1
        let count = match &row.stall {
            Some((st, _, c)) if naming::parse_status(st) == status => *c + 1,
            _ => 1,
        };
        row.stall = Some((naming::status_name(status).to_string(), at_ms, count));
    }

    fn clear_stall(&self, board: &str, number: u64) {
        let mut guard = self.state.lock().expect("state lock");
        if let Some(row) = guard.rows.get_mut(&Self::key(board, number)) {
            row.stall = None;
        }
    }

    fn lost_session(&self, board: &str, number: u64) -> Option<(Status, u32)> {
        let guard = self.state.lock().expect("state lock");
        guard
            .rows
            .get(&Self::key(board, number))
            .and_then(|r| r.lost.as_ref())
            .map(|(st, c)| (naming::parse_status(st), *c))
    }

    fn mark_lost_session(&self, board: &str, number: u64, status: Status) {
        let mut guard = self.state.lock().expect("state lock");
        let row = guard.rows.entry(Self::key(board, number)).or_default();
        // счётчик растёт внутри раунда (одинаковый статус); смена статуса = 1
        let count = match &row.lost {
            Some((st, c)) if naming::parse_status(st) == status => *c + 1,
            _ => 1,
        };
        row.lost = Some((naming::status_name(status).to_string(), count));
    }

    fn clear_lost_session(&self, board: &str, number: u64) {
        let mut guard = self.state.lock().expect("state lock");
        if let Some(row) = guard.rows.get_mut(&Self::key(board, number)) {
            row.lost = None;
        }
    }

    fn set_last_comment_id(&self, board: &str, number: u64, id: &str) {
        let mut guard = self.state.lock().expect("state lock");
        guard
            .rows
            .entry(Self::key(board, number))
            .or_default()
            .last_comment_id = Some(id.to_string());
    }

    fn dreamer_mark(&self, session: &str) -> Option<DreamerMark> {
        self.state
            .lock()
            .expect("state lock")
            .dreamer
            .get(session)
            .copied()
    }

    fn mark_dreamer(&self, session: &str, mark: DreamerMark) {
        self.state
            .lock()
            .expect("state lock")
            .dreamer
            .insert(session.to_string(), mark);
    }

    fn dreamer_session(&self, source: &str) -> Option<String> {
        self.state
            .lock()
            .expect("state lock")
            .dreamer_links
            .get(source)
            .cloned()
    }

    fn link_dreamer_session(&self, source: &str, dreamer: &str) {
        self.state
            .lock()
            .expect("state lock")
            .dreamer_links
            .insert(source.to_string(), dreamer.to_string());
    }

    fn prune_dreamer(&self, live: &std::collections::HashSet<String>) {
        let mut guard = self.state.lock().expect("state lock");
        guard.dreamer.retain(|id, _| live.contains(id));
        guard
            .dreamer_links
            .retain(|source, _| live.contains(source));
    }

    fn dreamer_day_spawns(&self, day_key: &str) -> u32 {
        self.state
            .lock()
            .expect("state lock")
            .dreamer_day
            .as_ref()
            .filter(|(d, _)| d == day_key)
            .map(|(_, n)| *n)
            .unwrap_or(0)
    }

    fn mark_dreamer_spawn(&self, day_key: &str) {
        let mut guard = self.state.lock().expect("state lock");
        let n = guard
            .dreamer_day
            .as_ref()
            .filter(|(d, _)| d == day_key)
            .map(|(_, n)| *n)
            .unwrap_or(0);
        guard.dreamer_day = Some((day_key.to_string(), n + 1));
    }

    fn last_spawn_ms(&self, board: &str, number: u64, slot: Slot) -> Option<u64> {
        let key = format!("{board}:{number}:{}", naming::slot_key(slot));
        self.state
            .lock()
            .expect("state lock")
            .spawns
            .get(&key)
            .copied()
    }

    fn clear_spawn_ms(&self, board: &str, number: u64, slot: Slot) {
        self.state
            .lock()
            .expect("state lock")
            .spawns
            .remove(&format!("{board}:{number}:{}", naming::slot_key(slot)));
    }

    fn mark_spawn(&self, board: &str, number: u64, slot: Slot, at_ms: u64) {
        self.state.lock().expect("state lock").spawns.insert(
            format!("{board}:{number}:{}", naming::slot_key(slot)),
            at_ms,
        );
    }

    fn round_reentry_pending(&self, board: &str, number: u64) -> bool {
        self.state
            .lock()
            .expect("state lock")
            .rows
            .get(&Self::key(board, number))
            .map(|r| r.round_reentry)
            .unwrap_or(false)
    }

    fn set_round_reentry_pending(&self, board: &str, number: u64) {
        self.state
            .lock()
            .expect("state lock")
            .rows
            .entry(Self::key(board, number))
            .or_default()
            .round_reentry = true;
    }

    fn clear_round_reentry_pending(&self, board: &str, number: u64) {
        let mut guard = self.state.lock().expect("state lock");
        if let Some(row) = guard.rows.get_mut(&Self::key(board, number)) {
            row.round_reentry = false;
        }
    }

    fn last_analys_ms(&self, board: &str, number: u64) -> Option<u64> {
        self.state
            .lock()
            .expect("state lock")
            .spawns
            .get(&format!("{board}:{number}:analys"))
            .copied()
    }

    fn mark_analys_spawn(&self, board: &str, number: u64, at_ms: u64) {
        self.state
            .lock()
            .expect("state lock")
            .spawns
            .insert(format!("{board}:{number}:analys"), at_ms);
    }

    fn last_analys_review_ms(&self, board: &str, number: u64) -> Option<u64> {
        self.state
            .lock()
            .expect("state lock")
            .spawns
            .get(&format!("{board}:{number}:analys-review"))
            .copied()
    }

    fn mark_analys_review_spawn(&self, board: &str, number: u64, at_ms: u64) {
        self.state
            .lock()
            .expect("state lock")
            .spawns
            .insert(format!("{board}:{number}:analys-review"), at_ms);
    }

    fn analys_loop(&self, board: &str, number: u64) -> Option<(Status, u32)> {
        self.state
            .lock()
            .expect("state lock")
            .analys_loops
            .get(&Self::key(board, number))
            .map(|(s, r)| (naming::parse_status(s), *r))
    }

    fn set_analys_loop(&self, board: &str, number: u64, status: Status, rounds: u32) {
        self.state.lock().expect("state lock").analys_loops.insert(
            Self::key(board, number),
            (naming::status_name(status).to_string(), rounds),
        );
    }

    fn clear_analys_loop(&self, board: &str, number: u64) {
        self.state
            .lock()
            .expect("state lock")
            .analys_loops
            .remove(&Self::key(board, number));
    }

    fn analys_review_loop(&self, board: &str, number: u64) -> Option<(Status, u32)> {
        self.state
            .lock()
            .expect("state lock")
            .spawn_loops
            .get(&format!("{board}:{number}:analys-review-loop"))
            .map(|(s, r)| (naming::parse_status(s), *r))
    }

    fn set_analys_review_loop(&self, board: &str, number: u64, status: Status, repeats: u32) {
        self.state.lock().expect("state lock").spawn_loops.insert(
            format!("{board}:{number}:analys-review-loop"),
            (naming::status_name(status).to_string(), repeats),
        );
    }

    fn clear_analys_review_loop(&self, board: &str, number: u64) {
        self.state
            .lock()
            .expect("state lock")
            .spawn_loops
            .remove(&format!("{board}:{number}:analys-review-loop"));
    }

    fn prune_analys_guards(&self, live: &std::collections::HashSet<String>) {
        let mut guard = self.state.lock().expect("state lock");
        guard.analys_loops.retain(|k, _| live.contains(k));
        let suffix = ":analys-review-loop";
        let dead: Vec<String> = guard
            .spawn_loops
            .keys()
            .filter(|k| k.ends_with(suffix))
            .filter(|k| !live.contains(k.strip_suffix(suffix).expect("суффикс проверен")))
            .cloned()
            .collect();
        for k in dead {
            guard.spawn_loops.remove(&k);
        }
    }

    fn spawn_loop(&self, board: &str, number: u64, slot: Slot) -> Option<(Status, u32)> {
        self.state
            .lock()
            .expect("state lock")
            .spawn_loops
            .get(&format!("{board}:{number}:{}", naming::slot_key(slot)))
            .map(|(s, r)| (naming::parse_status(s), *r))
    }

    fn set_spawn_loop(&self, board: &str, number: u64, slot: Slot, status: Status, repeats: u32) {
        self.state.lock().expect("state lock").spawn_loops.insert(
            format!("{board}:{number}:{}", naming::slot_key(slot)),
            (naming::status_name(status).to_string(), repeats),
        );
    }

    fn clear_spawn_loop(&self, board: &str, number: u64, slot: Slot) {
        self.state
            .lock()
            .expect("state lock")
            .spawn_loops
            .remove(&format!("{board}:{number}:{}", naming::slot_key(slot)));
    }

    fn reset_stale_spawn_loops(&self, board: &str, number: u64, current: Status) {
        let prefix = format!("{board}:{number}:");
        let current_name = naming::status_name(current);
        let mut guard = self.state.lock().expect("state lock");
        let stale: Vec<String> = guard
            .spawn_loops
            .iter()
            .filter(|(k, (status, _))| k.starts_with(&prefix) && status != current_name)
            .map(|(k, _)| k.clone())
            .collect();
        for key in stale {
            guard.spawn_loops.remove(&key);
        }
    }

    fn review_rounds(&self, board: &str, number: u64) -> u32 {
        let key = Self::key(board, number);
        self.state
            .lock()
            .expect("state lock")
            .review_rounds
            .get(&key)
            .copied()
            .unwrap_or(0)
    }

    fn set_review_rounds(&self, board: &str, number: u64, rounds: u32) {
        let key = Self::key(board, number);
        self.state
            .lock()
            .expect("state lock")
            .review_rounds
            .insert(key, rounds);
    }

    fn own_comment(&self, id: &str) -> bool {
        self.state
            .lock()
            .expect("state lock")
            .own_comments
            .contains_key(id)
    }

    fn mark_own_comment(&self, id: &str, posted_ms: u64) {
        self.state
            .lock()
            .expect("state lock")
            .own_comments
            .insert(id.to_string(), posted_ms);
    }

    fn master_binding(&self, role: &str, project: &str) -> Option<(String, String, String)> {
        let key = format!("{role}:{project}");
        self.state
            .lock()
            .expect("state lock")
            .master
            .get(&key)
            .and_then(
                |r| match (r.session.clone(), r.model.clone(), r.checkout.clone()) {
                    (Some(s), Some(m), Some(c)) => Some((s, m, c)),
                    _ => None,
                },
            )
    }

    fn set_master_binding(
        &self,
        role: &str,
        project: &str,
        session: &str,
        model_raw: &str,
        checkout: &str,
    ) {
        let key = format!("{role}:{project}");
        let mut guard = self.state.lock().expect("state lock");
        let row = guard.master.entry(key).or_default();
        row.session = Some(session.to_string());
        row.model = Some(model_raw.to_string());
        row.checkout = Some(checkout.to_string());
    }

    fn master_compact_ms(&self, role: &str, project: &str) -> u64 {
        let key = format!("{role}:{project}");
        self.state
            .lock()
            .expect("state lock")
            .master
            .get(&key)
            .and_then(|r| r.compact_ms)
            .unwrap_or(0)
    }

    fn set_master_compact_ms(&self, role: &str, project: &str, ms: u64) {
        let key = format!("{role}:{project}");
        self.state
            .lock()
            .expect("state lock")
            .master
            .entry(key)
            .or_default()
            .compact_ms = Some(ms);
    }

    fn prune_master_role(
        &self,
        role: &str,
        live_projects: &std::collections::HashSet<String>,
    ) -> bool {
        let prefix = format!("{role}:");
        let mut guard = self.state.lock().expect("state lock");
        let dead: Vec<String> = guard
            .master
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .filter(|k| {
                k.strip_prefix(&prefix)
                    .is_none_or(|p| !live_projects.contains(p))
            })
            .cloned()
            .collect();
        let pruned = !dead.is_empty();
        for k in dead {
            guard.master.remove(&k);
        }
        pruned
    }

    fn save(&self) -> UsecaseResult<()> {
        self.save_now()
    }
}

impl BotStateStore for JsonStateStore {
    fn bot_session(&self, slug: &str) -> Option<String> {
        self.state
            .lock()
            .expect("state lock")
            .bots
            .get(slug)
            .and_then(|b| b.session.clone())
    }

    fn set_bot_session(&self, slug: &str, session: Option<&str>) {
        let mut guard = self.state.lock().expect("state lock");
        match session {
            // старая сессия в opencode.db живёт — забывается только ссылка
            Some(s) => guard.bots.entry(slug.to_string()).or_default().session = Some(s.into()),
            None => {
                if let Some(row) = guard.bots.get_mut(slug) {
                    row.session = None;
                }
            }
        }
    }

    fn bot_offset(&self, slug: &str) -> Option<u64> {
        self.state
            .lock()
            .expect("state lock")
            .bots
            .get(slug)
            .and_then(|b| b.offset)
    }

    fn bot_manual_hash(&self, slug: &str) -> Option<u64> {
        self.state
            .lock()
            .expect("state lock")
            .bots
            .get(slug)
            .and_then(|b| b.manual_hash)
    }

    fn set_bot_manual_hash(&self, slug: &str, hash: Option<u64>) {
        let mut guard = self.state.lock().expect("state lock");
        match guard.bots.get_mut(slug) {
            Some(row) => row.manual_hash = hash,
            // пустой строке нечего забывать — Some завёл бы сироту
            None if hash.is_some() => {
                guard.bots.entry(slug.to_string()).or_default().manual_hash = hash;
            }
            None => {}
        }
    }

    fn bot_lessons_hash(&self, slug: &str) -> Option<u64> {
        self.state
            .lock()
            .expect("state lock")
            .bots
            .get(slug)
            .and_then(|b| b.lessons_hash)
    }

    fn set_bot_lessons_hash(&self, slug: &str, hash: Option<u64>) {
        let mut guard = self.state.lock().expect("state lock");
        match guard.bots.get_mut(slug) {
            Some(row) => row.lessons_hash = hash,
            // пустой строке нечего забывать — Some завёл бы сироту
            None if hash.is_some() => {
                guard.bots.entry(slug.to_string()).or_default().lessons_hash = hash;
            }
            None => {}
        }
    }

    fn bot_compact_ms(&self, slug: &str) -> u64 {
        self.state
            .lock()
            .expect("state lock")
            .bots
            .get(slug)
            .and_then(|b| b.compact_ms)
            .unwrap_or(0)
    }

    fn set_bot_compact_ms(&self, slug: &str, ms: u64) {
        self.state
            .lock()
            .expect("state lock")
            .bots
            .entry(slug.to_string())
            .or_default()
            .compact_ms = Some(ms);
    }

    fn set_bot_offset(&self, slug: &str, offset: u64) {
        self.state
            .lock()
            .expect("state lock")
            .bots
            .entry(slug.to_string())
            .or_default()
            .offset = Some(offset);
    }

    fn save(&self) -> UsecaseResult<()> {
        self.save_now()
    }
}

impl CronStateStore for JsonStateStore {
    fn cron_row(&self, name: &str) -> CronRow {
        self.state
            .lock()
            .expect("state lock")
            .crons
            .get(name)
            .cloned()
            .map(Into::into)
            .unwrap_or_default()
    }

    fn set_cron_row(&self, name: &str, row: CronRow) {
        self.state
            .lock()
            .expect("state lock")
            .crons
            .insert(name.to_string(), CronRowState::from(&row));
    }

    fn save(&self) -> UsecaseResult<()> {
        self.save_now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::SystemClock;

    /// Часы-константа: тестовые метки задаются относительно «сейчас».
    struct FixedTestClock(u64);
    impl Clock for FixedTestClock {
        fn utc_minute(&self) -> uwuwu_domain::UtcMinute {
            uwuwu_domain::UtcMinute {
                minute_of_day: 0,
                weekday: 1,
            }
        }
        fn now_ms(&self) -> u64 {
            self.0
        }
    }

    fn store(path: PathBuf) -> JsonStateStore {
        JsonStateStore::load(path, Arc::new(SystemClock))
    }

    fn store_at(path: PathBuf, now_ms: u64) -> JsonStateStore {
        JsonStateStore::load(path, Arc::new(FixedTestClock(now_ms)))
    }

    #[test]
    fn round_trip_through_file() {
        let dir = std::env::temp_dir().join(format!("uwuwu-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        {
            let s = store(path.clone());
            s.set_last_comment_id("Задачи uwuwu", 7, "c1");
            // бот: сессия + offset переживают рестарт (секция bots)
            s.set_bot_session("bot/x", Some("ses_abc"));
            s.set_bot_offset("bot/x", 42);
            // крон: машинный учёт рана переживает рестарт (секция crons, #1480)
            CronStateStore::set_cron_row(
                &s,
                "qa-night",
                CronRow {
                    last_fire_ms: Some(1_000),
                    anchor_ms: Some(900),
                    deferred_since_ms: None,
                    run: Some(CronRunTrack {
                        session: "ses_run".into(),
                        fired_ms: 1_000,
                    }),
                    fails: 2,
                },
            );
            TickStateStore::save(&s).unwrap();
        }
        let s = store(path.clone());
        assert_eq!(s.last_comment_id("Задачи uwuwu", 7).as_deref(), Some("c1"));
        assert_eq!(
            s.bot_session("bot/x").as_deref(),
            Some("ses_abc"),
            "рестарт восстанавливает сессию бота"
        );
        assert_eq!(s.bot_offset("bot/x"), Some(42));
        let row = s.cron_row("qa-night");
        assert_eq!(
            row.last_fire_ms,
            Some(1_000),
            "учёт рана переживает рестарт"
        );
        assert_eq!(row.anchor_ms, Some(900), "якорь переживает рестарт (#1517)");
        assert_eq!(
            row.run.map(|r| r.session).as_deref(),
            Some("ses_run"),
            "run-трек переживает рестарт"
        );
        assert_eq!(row.fails, 2, "счётчик отказов переживает рестарт");
        assert_eq!(s.cron_row("нет-такого").fails, 0, "нет записи — дефолт");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lost_counter_persists_across_restart_and_resets_on_status_change() {
        // #1288: счётчик потерянных доставок — факт носителя (не процесса),
        // персистентен: переживает рестарт сервера; статус-тегирование —
        // смена статуса строки открывает новый раунд (счётчик с 1)
        let dir = std::env::temp_dir().join(format!("uwuwu-state-lost-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        {
            let s = store(path.clone());
            TickStateStore::mark_lost_session(&s, "Задачи uwuwu", 7, Status::InProgress);
            TickStateStore::mark_lost_session(&s, "Задачи uwuwu", 7, Status::InProgress);
            TickStateStore::save(&s).unwrap();
        }
        let s = store(path.clone());
        assert_eq!(
            TickStateStore::lost_session(&s, "Задачи uwuwu", 7),
            Some((Status::InProgress, 2)),
            "рестарт восстанавливает счётчик потери (факт носителя)"
        );
        // смена статуса строки = новый раунд
        TickStateStore::mark_lost_session(&s, "Задачи uwuwu", 7, Status::Design);
        assert_eq!(
            TickStateStore::lost_session(&s, "Задачи uwuwu", 7),
            Some((Status::Design, 1))
        );
        // сброс: успешная доставка / парковка
        TickStateStore::clear_lost_session(&s, "Задачи uwuwu", 7);
        assert_eq!(TickStateStore::lost_session(&s, "Задачи uwuwu", 7), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn round_reentry_persists_across_restart_without_noise() {
        // #1626 (тестер-пин, интеграция): pending-маркер раунд-промпта —
        // факт носителя: переживает рестарт сервера; снятый маркер не
        // шумит в state.json (skip_serializing_if-конвенция RowState,
        // дельта-фикс ревью).
        let dir = std::env::temp_dir().join(format!("uwuwu-state-reentry-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        {
            let s = store(path.clone());
            s.set_round_reentry_pending("Задачи uwuwu", 9);
            TickStateStore::save(&s).unwrap();
        }
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("round_reentry"),
            "взведённый маркер персистентен в носителе"
        );
        let s = store(path.clone());
        assert!(
            s.round_reentry_pending("Задачи uwuwu", 9),
            "рестарт восстанавливает pending-маркер (факт носителя)"
        );
        assert!(
            !s.round_reentry_pending("Задачи uwuwu", 10),
            "чужая строка без маркера"
        );
        s.clear_round_reentry_pending("Задачи uwuwu", 9);
        TickStateStore::save(&s).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("round_reentry"),
            "снятый маркер не шумит в носителе"
        );
        let s = store(path.clone());
        assert!(
            !s.round_reentry_pending("Задачи uwuwu", 9),
            "снятие переживает рестарт"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn own_comments_persist_across_restart() {
        // #1204: реестр серверных постов переживает рестарт — комменты,
        // опубликованные до перезапуска, остаются «своими»
        let dir = std::env::temp_dir().join(format!("uwuwu-state-own-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        let week_ms = 7 * 24 * 60 * 60 * 1000;
        {
            let s = store_at(path.clone(), 10 * week_ms);
            s.mark_own_comment("MAC_own1", 10 * week_ms - 1000);
            s.mark_own_comment("MAC_own2", 10 * week_ms - 2000);
            TickStateStore::save(&s).unwrap();
        }
        let s = store(path.clone());
        assert!(s.own_comment("MAC_own1"), "марка переживает рестарт");
        assert!(s.own_comment("MAC_own2"));
        assert!(!s.own_comment("MAC_user"), "чужой id не в реестре");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn own_comments_read_legacy_file_without_field() {
        // #1204 Low-3: state.json, записанный пре-фиксом (без own_comments),
        // читается — serde-дефолт пустого реестра, рестарт не падает
        let dir = std::env::temp_dir().join(format!("uwuwu-state-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        std::fs::write(
            &path,
            r#"{"rows":{},"dreamer_done":[],"spawns":{},"spawn_loops":{},"bots":{}}"#,
        )
        .unwrap();
        let week_ms = 7 * 24 * 60 * 60 * 1000;
        let s = store_at(path.clone(), 10 * week_ms);
        assert!(!s.own_comment("anything"), "легаси-файл — реестр пуст");
        s.mark_own_comment("MAC_new", 10 * week_ms - 500);
        TickStateStore::save(&s).unwrap();
        let s = store(path.clone());
        assert!(
            s.own_comment("MAC_new"),
            "марка доезжает после легаси-чтения"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dreamer_marks_and_links_persist_across_restart() {
        // #1306: watermark-метки и связки «исходная → дример-сессия»
        // переживают рестарт — revival-дообработка не теряется.
        let dir = std::env::temp_dir().join(format!("uwuwu-state-dream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        {
            let s = store(path.clone());
            s.mark_dreamer("ses_a", DreamerMark::UpTo(100_500));
            s.mark_dreamer("ses_bot", DreamerMark::Forever);
            s.link_dreamer_session("ses_a", "ses_dream_a");
            TickStateStore::save(&s).unwrap();
        }
        let s = store(path.clone());
        assert_eq!(
            s.dreamer_mark("ses_a"),
            Some(DreamerMark::UpTo(100_500)),
            "watermark переживает рестарт"
        );
        assert_eq!(s.dreamer_mark("ses_bot"), Some(DreamerMark::Forever));
        assert_eq!(
            s.dreamer_session("ses_a").as_deref(),
            Some("ses_dream_a"),
            "линк носителя переживает рестарт"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn legacy_dreamer_done_migrates_to_position_marks() {
        // #1306: legacy вечных маркеров (Vec) мигрирует при загрузке в
        // UpTo(момент загрузки) — post-deploy revival дообрабатывается,
        // старые сессии молчат; поле в новом файле не пишется.
        let dir = std::env::temp_dir().join(format!("uwuwu-state-dream-lg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        std::fs::write(
            &path,
            r#"{"rows":{},"dreamer_done":["ses_old1","ses_old2"],"spawns":{}}"#,
        )
        .unwrap();
        let week_ms = 7 * 24 * 60 * 60 * 1000;
        let s = store_at(path.clone(), 10 * week_ms);
        assert_eq!(
            s.dreamer_mark("ses_old1"),
            Some(DreamerMark::UpTo(10 * week_ms)),
            "legacy-маркер = позиция момента загрузки"
        );
        assert_eq!(
            s.dreamer_mark("ses_old2"),
            Some(DreamerMark::UpTo(10 * week_ms))
        );
        s.mark_dreamer("ses_new", DreamerMark::UpTo(10 * week_ms + 500));
        TickStateStore::save(&s).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("\"dreamer_done\""),
            "legacy-поле в новом файле не пишется"
        );
        assert!(raw.contains("\"dreamer\""), "новая секция меток на месте");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bot_keys_migrated_to_flat_layout_on_load() {
        // #1214: переезд носителя ботов — ключи state `Projects/*/Боты/X`
        // → `bot/X` при загрузке: сессия и offset переживают переезд;
        // существующий новый ключ приоритетен (свежее не перетирается).
        let dir = std::env::temp_dir().join(format!("uwuwu-state-bot-mig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        std::fs::write(
            &path,
            r#"{"rows":{},"spawns":{},
                "bots":{"Projects/uwuwu/Боты/Помощник":{"session":"ses_old","offset":7},
                        "bot/Помощник":{"session":"ses_new"}}}"#,
        )
        .unwrap();
        let s = store_at(path.clone(), 0);
        {
            let guard = s.state.lock().expect("state lock");
            let bots = &guard.bots;
            assert!(
                !bots.contains_key("Projects/uwuwu/Боты/Помощник"),
                "старый ключ вынесен: {bots:?}"
            );
            let migrated = bots.get("bot/Помощник").expect("новый ключ на месте");
            assert_eq!(migrated.session.as_deref(), Some("ses_new"));
            assert!(migrated.offset.is_none(), "новый ключ приоритетен");
        }
        // чистая миграция: старый ключ без соседа переносится целиком
        let other = dir.join("state2.json");
        std::fs::write(
            &other,
            r#"{"rows":{},"spawns":{},
                "bots":{"Projects/origa/Боты/Маркетинг":{"session":"ses_m","offset":3}}}"#,
        )
        .unwrap();
        let s2 = store_at(other, 0);
        {
            let guard = s2.state.lock().expect("state lock");
            let row = guard.bots.get("bot/Маркетинг").expect("ключ перенесён");
            assert_eq!(row.session.as_deref(), Some("ses_m"));
            assert_eq!(row.offset, Some(3));
            assert!(
                !guard.bots.contains_key("Projects/origa/Боты/Маркетинг"),
                "старый ключ вынесен"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn legacy_migration_keeps_forever_skip_for_bot_sessions() {
        // #1306 (ревью, high): в legacy-векторе лежали и бот-сессии
        // (старый раннер маркировал их как вечный скип). Когорта ботов —
        // по реестру `bots[*].session` — сохраняет Forever: иначе живой
        // бот после деплоя становится перманентным кандидатом дримера
        // (каждое сообщение поднимает updated_ms выше позиции).
        let dir =
            std::env::temp_dir().join(format!("uwuwu-state-dream-lgb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        std::fs::write(
            &path,
            r#"{"rows":{},"dreamer_done":["ses_bot_alive","ses_work"],
                "spawns":{},
                "bots":{"Projects/uwuwu/Боты/Помощник":{"session":"ses_bot_alive","offset":5}}}"#,
        )
        .unwrap();
        let week_ms = 7 * 24 * 60 * 60 * 1000;
        let s = store_at(path.clone(), 10 * week_ms);
        assert_eq!(
            s.dreamer_mark("ses_bot_alive"),
            Some(DreamerMark::Forever),
            "легаси-бот — вечный скип, не кандидат даже при активности"
        );
        assert_eq!(
            s.dreamer_mark("ses_work"),
            Some(DreamerMark::UpTo(10 * week_ms)),
            "рабочая сессия — позиция момента загрузки"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn own_comments_gc_prunes_by_age() {
        // #1204: записи старше недели срезаются при сохранении (журнал
        // комментов держит последнюю сотню — старше не нужно)
        let dir = std::env::temp_dir().join(format!("uwuwu-state-gc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        let week_ms = 7 * 24 * 60 * 60 * 1000;
        let s = store_at(path.clone(), 10 * week_ms);
        s.mark_own_comment("MAC_fresh", 10 * week_ms - 1000);
        s.mark_own_comment("MAC_stale", week_ms - 1000);
        TickStateStore::save(&s).unwrap();
        assert!(s.own_comment("MAC_fresh"), "свежая марка жива");
        assert!(!s.own_comment("MAC_stale"), "недельная марка срезана");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bot_session_clear_keeps_offset_row() {
        let s = store(PathBuf::from("/nonexistent/uwuwu/x.json"));
        s.set_bot_session("slug", Some("ses_1"));
        s.set_bot_offset("slug", 9);
        s.set_bot_session("slug", None);
        assert_eq!(s.bot_session("slug"), None, "сессия забыта");
        assert_eq!(
            s.bot_offset("slug"),
            Some(9),
            "offset переживает пересоздание"
        );
    }

    #[test]
    fn missing_file_is_fresh_state() {
        let s = store(PathBuf::from("/nonexistent/uwuwu/x.json"));
        assert!(s.last_comment_id("b", 1).is_none());
        assert!(s.bot_session("slug").is_none());
        assert!(s.bot_offset("slug").is_none());
    }

    #[test]
    fn review_rounds_persist_across_restart() {
        // #1311: счётчик подряд request_changes переживает рестарт сервера —
        // иначе любой рестарт обнулял бы стоп-кран ревью-цикла (урок ADR-107)
        let dir = std::env::temp_dir().join(format!("uwuwu-state-rr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        {
            let s = store(path.clone());
            s.set_review_rounds("Задачи uwuwu", 901, 3);
            s.set_review_rounds("Задачи uwuwu", 902, 0);
            TickStateStore::save(&s).unwrap();
        }
        let s = store(path.clone());
        assert_eq!(
            s.review_rounds("Задачи uwuwu", 901),
            3,
            "счётчик переживает рестарт"
        );
        assert_eq!(
            s.review_rounds("Задачи uwuwu", 902),
            0,
            "явный ноль (сброс) не превращается в отсутствие ключа"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn review_rounds_gc_caps_the_map() {
        // #1311: GC-кап 5000 в save_now — карта счётчиков не растёт вечно
        // (сиблинг-семантика spawn_loops/own_comments); срез — старейший
        // ключ (BTreeMap-порядок)
        let dir = std::env::temp_dir().join(format!("uwuwu-state-rr-gc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        let s = store(path.clone());
        for n in 1..=5001u64 {
            s.set_review_rounds("B", n, 7);
        }
        TickStateStore::save(&s).unwrap();
        let s = store(path);
        assert_eq!(s.review_rounds("B", 5001), 7, "свежий ключ жив");
        assert_eq!(s.review_rounds("B", 1), 0, "старейший ключ срезан капом");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bot_lessons_hash_round_trip_and_orphan_reset() {
        // #1424 (тестер): lessons-hash (agent-режим) переживает рестарт;
        // Some(0) — «носитель опустел, уроки отозваны» — не схлопывается
        // в None (иначе следующий ход ложно доставит уроки снова);
        // сброс None на несуществующей строке не заводит сироту-строку.
        let dir = std::env::temp_dir().join(format!("uwuwu-state-lessons-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        {
            let s = store(path.clone());
            assert_eq!(
                s.bot_lessons_hash("bot/a"),
                None,
                "старый state без поля — уроков нет"
            );
            s.set_bot_lessons_hash("bot/a", Some(0)); // отзыв: hash пустого носителя
            s.set_bot_lessons_hash("bot/b", Some(77));
            TickStateStore::save(&s).unwrap();
        }
        let s = store(path.clone());
        assert_eq!(
            s.bot_lessons_hash("bot/a"),
            Some(0),
            "Some(0) переживает рестарт и не читается как None"
        );
        assert_eq!(s.bot_lessons_hash("bot/b"), Some(77));
        s.set_bot_lessons_hash("bot/ghost", None);
        TickStateStore::save(&s).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let bots = json.get("bots").expect("секция bots");
        assert!(
            bots.get("bot/ghost").is_none(),
            "сброс None на несуществующей строке не заводит сироту: {bots}"
        );
        assert!(
            bots.get("bot/a")
                .expect("строка a жива")
                .get("lessons_hash")
                .is_some(),
            "lessons_hash сериализуется в state.json"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_state_preserved_and_starts_empty() {
        // #1517: битый state.json — громкий старт с пустым state, улика
        // рядом со штампом; старые улики сносятся (удержание одной).
        let dir = std::env::temp_dir().join(format!("uwuwu-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        std::fs::write(&path, "{ не json").unwrap();
        std::fs::write(dir.join("state.json.corrupt-1"), "старая улика").unwrap();
        let s = store_at(path.clone(), 5_000);
        assert_eq!(
            CronStateStore::cron_row(&s, "любой"),
            CronRow::default(),
            "битый файл — старт с пустым state"
        );
        assert!(
            dir.join("state.json.corrupt-5").exists(),
            "улика сохранена со штампом (now_ms/1000)"
        );
        assert!(
            !dir.join("state.json.corrupt-1").exists(),
            "старая улика снесена"
        );
        // после сброса запись работает штатно
        CronStateStore::set_cron_row(&s, "x", CronRow::default());
        TickStateStore::save(&s).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        serde_json::from_str::<serde_json::Value>(&raw).expect("файл снова валиден");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn binary_garbage_state_is_loud_corruption_not_quiet_start() {
        // #1517 (ревью Common): невалидный UTF-8 — реалистичный режим
        // порчи (усечённая запись/сбой тома); чтение в bytes доходит до
        // парсера → громкая коррупция + улика, не тихий пустой старт.
        let dir = std::env::temp_dir().join(format!("uwuwu-binary-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        std::fs::write(&path, [0xFF, 0xFE, 0x00, 0xC3, 0x28, 0x7B]).unwrap();
        let s = store_at(path.clone(), 9_000);
        assert_eq!(CronStateStore::cron_row(&s, "x"), CronRow::default());
        assert!(
            dir.join("state.json.corrupt-9").exists(),
            "бинарный мусор сохранён уликой"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_state_file_starts_quietly() {
        // Нет файла — первый старт, тихо и без улик.
        let dir = std::env::temp_dir().join(format!("uwuwu-missing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        let s = store_at(path.clone(), 1_000);
        assert_eq!(CronStateStore::cron_row(&s, "x"), CronRow::default());
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("corrupt"))
            .collect();
        assert!(leftovers.is_empty(), "улик нет: {leftovers:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn concurrent_saves_leave_valid_file() {
        // #1517: save из разных тасков (тик-сайдкары × бот-раннеры)
        // сериализован мьютексом — файл после гонки валиден, tmp один.
        let dir = std::env::temp_dir().join(format!("uwuwu-race-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        let s = Arc::new(store(path.clone()));
        let handles: Vec<_> = (0..8)
            .map(|n| {
                let s = s.clone();
                std::thread::spawn(move || {
                    for i in 0..20 {
                        CronStateStore::set_cron_row(
                            &*s,
                            &format!("cron-{n}"),
                            CronRow {
                                last_fire_ms: Some((n * 100 + i) as u64),
                                ..CronRow::default()
                            },
                        );
                        CronStateStore::save(&*s).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let raw = std::fs::read_to_string(&path).unwrap();
        serde_json::from_str::<serde_json::Value>(&raw).expect("state.json валиден после гонки");
        assert_eq!(
            CronStateStore::cron_row(&*s, "cron-7").last_fire_ms,
            Some(719)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn master_binding_persists_across_restart_and_prunes_orphans() {
        // #1634 (тестер-пин, интеграция): привязка роль:проект и метка
        // компакта — факты носителя (не процесса): переживают рестарт;
        // prune сносит только сирот своей роли.
        let dir = std::env::temp_dir().join(format!("uwuwu-state-master-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        {
            let s = store(path.clone());
            s.set_master_binding(
                "analyst-reviewer",
                "uwuwu",
                "ses_live",
                "zai-coding-plan/glm-5.4#high",
                "/tmp/checkout",
            );
            s.set_master_compact_ms("analyst-reviewer", "uwuwu", 123_456);
            s.set_master_binding("analyst-reviewer", "gone", "ses_gone", "m", "/c");
            s.set_master_binding("analyst", "gone", "ses_a", "m", "/c");
            TickStateStore::save(&s).unwrap();
        }
        let s = store(path.clone());
        assert_eq!(
            s.master_binding("analyst-reviewer", "uwuwu"),
            Some((
                "ses_live".to_string(),
                "zai-coding-plan/glm-5.4#high".to_string(),
                "/tmp/checkout".into()
            )),
            "рестарт восстанавливает тройку привязки"
        );
        assert_eq!(
            s.master_compact_ms("analyst-reviewer", "uwuwu"),
            123_456,
            "метка компакта переживает рестарт"
        );
        assert_eq!(
            s.master_compact_ms("analyst-reviewer", "нет-такой"),
            0,
            "нет записи — метка ноль"
        );
        let mut live = std::collections::HashSet::new();
        live.insert("uwuwu".to_string());
        assert!(
            s.prune_master_role("analyst-reviewer", &live),
            "сирота есть — prune докладывает"
        );
        assert!(
            !s.prune_master_role("analyst-reviewer", &live),
            "второй проход чист — prune молчит"
        );
        TickStateStore::save(&s).unwrap();
        let s = store(path.clone());
        assert_eq!(
            s.master_binding("analyst-reviewer", "gone"),
            None,
            "сирота своей роли снесена"
        );
        assert!(
            s.master_binding("analyst-reviewer", "uwuwu").is_some(),
            "живой проект цел"
        );
        assert!(
            s.master_binding("analyst", "gone").is_some(),
            "чужая роль не тронута"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn master_binding_reads_legacy_file_without_section() {
        // #1634: state.json, записанный до мастер-привязок (без master),
        // читается — serde-дефолт пустой карты, рестарт не падает.
        let dir = std::env::temp_dir().join(format!("uwuwu-state-mlegacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        std::fs::write(&path, r#"{"rows":{},"bots":{}}"#).unwrap();
        let s = store(path.clone());
        assert_eq!(
            s.master_binding("analyst-reviewer", "uwuwu"),
            None,
            "легаси-файл — привязок нет"
        );
        assert_eq!(s.master_compact_ms("analyst-reviewer", "uwuwu"), 0);
        s.set_master_binding("analyst-reviewer", "uwuwu", "ses_1", "m", "/c");
        TickStateStore::save(&s).unwrap();
        let s = store(path.clone());
        assert!(
            s.master_binding("analyst-reviewer", "uwuwu").is_some(),
            "привязка доезжает после легаси-чтения"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
