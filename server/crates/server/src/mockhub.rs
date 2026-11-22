//! Мок-hub: доска/issues/поля в памяти (E2E-стенд без GitHub и rate-limit).
//! Включается конфигом [hub] driver = "memory". Совместим с остальным
//! контуром: тик, MCP-инструменты, мок-агенты — всё через порты.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;
use uwuwu_domain::{Priority, Slot, Status, Task, TaskFields};
use uwuwu_usecase::{AppendDocOutcome, UsecaseError, UsecaseResult};
use uwuwu_usecase::{CommentRef, IssueView};
use uwuwu_usecase::{IssueFieldStore, TaskBoardRepo};

/// Состояние мок-хаба: строки «доски» (все имена досок → один стенд).
#[derive(Default)]
struct HubState {
    next_number: u64,
    /// number → задача
    tasks: HashMap<u64, Task>,
    /// number → доска (тег; без тега — видна на всех досках стенда)
    board_tags: HashMap<u64, String>,
    /// number → поля (сессии/ветка/PR); Приоритет/Automerge — поле строки
    /// (#402/#1258: set_board_cells пишет в Task, load проектирует наружу)
    fields: HashMap<u64, TaskFields>,
    /// number → тело issue
    bodies: HashMap<u64, String>,
    /// number → комменты
    comments: HashMap<u64, Vec<CommentRef>>,
    /// number → issue closed
    closed: HashMap<u64, bool>,
    /// number → архивация строки (клин холодного ящика #1293):
    /// архивная строка покидает read_board, но задачи/тела живут
    archived: HashMap<u64, bool>,
    /// number → снос item'а (батч-чистка #1158): строка снята с доски
    /// (undo-журнал сервера возвращает — restore_row)
    removed: HashMap<u64, bool>,
    counter: u64,
    /// Слэг → markdown (вики стенда; seed UWUWU_MOCK_DOCS — JSON-карта).
    docs: HashMap<String, String>,
}

/// Seed-строка стенда: доска (опционально), номер, стартовый статус,
/// приоритет (опционально — дефолт Empty; носитель приоритета для
/// преемпшена CUP #1259, чьё решение живёт в строке доски), дни со
/// дня закрытия issue (опционально — вход клина холодного ящика #1293).
struct SeedRow {
    board: Option<String>,
    number: u64,
    status: Status,
    priority: Priority,
    days_closed: u64,
}

/// Парсинг UWUWU_MOCK_SEED: «;»-раздельные строки «board:number:status»
/// или «number:status» (без тега — видна на всех досках стенда);
/// хвост «:high|medium|low» задаёт приоритет (преемпшен #1259 e2e);
/// пятая часть «:days» — возраст закрытия issue (клин #1293 e2e).
/// Трёхчастные сиды дискриминируются первой частью: число →
/// «number:status:priority», иначе — «board:number:status» (доска
/// нечисловая; «In Progress» номером быть не может — парсер отсечёт).
fn parse_seeds(raw: &str) -> Vec<SeedRow> {
    raw.split(';')
        .filter_map(|entry| {
            let parts: Vec<&str> = entry.split(':').collect();
            let (board, number, status, prio, days) = match parts.as_slice() {
                [board, n, s, p, d] => (
                    Some((*board).trim().to_string()),
                    n,
                    s,
                    *p,
                    d.trim().parse::<u64>().unwrap_or(0),
                ),
                [board, n, s, p] => (Some((*board).trim().to_string()), n, s, *p, 0),
                [a, b, c] if a.trim().parse::<u64>().is_ok() => (None, a, b, *c, 0),
                // прочие трёхчастные — «board:number:status» (без тега с
                // приоритетом — 4-частная форма «board:number:status:pr»)
                [board, n, s] => (Some((*board).trim().to_string()), n, s, "", 0),
                [n, s] => (None, n, s, "", 0),
                _ => return None,
            };
            Some(SeedRow {
                board,
                number: number.trim().parse().ok()?,
                status: uwuwu_usecase::parse_status(status.trim()),
                priority: uwuwu_usecase::parse_priority(prio),
                days_closed: days,
            })
        })
        .collect()
}

/// Семейная ячейка слота: прод-носитель хранит одно значение поля на
/// семью — «Сессия dev» покрывает Design|Dev, «Сессия ревью» —
/// PlanReview|ResultReview (github/fields.rs; запись — mirror hub.rs
/// set_session/clear_session). Мок хранит канонический ключ семьи,
/// shadow-слоты разворачиваются на выдаче (expand_families).
fn family_cell(slot: Slot) -> Slot {
    match slot {
        Slot::Design => Slot::Dev,
        Slot::ResultReview => Slot::PlanReview,
        other => other,
    }
}

/// Разворот семейных ячеек на выдаче (mirror github/hub.rs
/// map_task_fields): одно значение поля видно обоим слотам семьи —
/// Design-строка видит свою сессию, а после перехода в In Progress
/// тот же сеанс читается в дев-слоте (прод-геометрия «переезда»
/// дев-сессии, #1237): тик не спавнит дубль, wake-путь получает
/// адресата.
fn expand_families(fields: &TaskFields) -> TaskFields {
    let mut out = fields.clone();
    for (canonical, shadow) in [
        (Slot::Dev, Slot::Design),
        (Slot::PlanReview, Slot::ResultReview),
    ] {
        if let Some(s) = out.session(canonical).cloned() {
            out.set_session(shadow, s);
        }
    }
    out
}

pub struct MemoryHub {
    state: Mutex<HubState>,
    /// seed-строки — создаются при пустой доске.
    seeds: Vec<SeedRow>,
}

impl MemoryHub {
    /// Seed-строки для E2E-прогона (UWUWU_MOCK_SEED), «;»-раздельные:
    /// «1148:Design» (без тега — видна на всех досках) или
    /// «Задачи uwuwu:1148:Design» (тег доски — мульти-экзекьютор #1203).
    /// Вики-сид: UWUWU_MOCK_DOCS — JSON «слаг → markdown» (боты e2e).
    pub fn from_env() -> Self {
        let seeds = std::env::var("UWUWU_MOCK_SEED")
            .ok()
            .map(|raw| parse_seeds(&raw))
            .unwrap_or_default();
        Self::with_seeds(seeds)
    }

    /// Явный сид (юнит-тесты): строка (номер, статус) на пустой доске,
    /// без тега — видна на всех досках стенда.
    #[cfg(test)]
    pub fn with_seed(seed: Option<(u64, Status)>) -> Self {
        Self::with_seeds(match seed {
            Some((number, status)) => vec![SeedRow {
                board: None,
                number,
                status,
                priority: Priority::Empty,
                days_closed: 0,
            }],
            None => Vec::new(),
        })
    }

    /// Общий конструктор: готовый набор сидов (мульти-досочный стенд #1203).
    fn with_seeds(seeds: Vec<SeedRow>) -> Self {
        let docs: HashMap<String, String> = std::env::var("UWUWU_MOCK_DOCS")
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        if !docs.is_empty() {
            tracing::info!(
                event = "mockhub.docs_seeded",
                count = docs.len(),
                "Вики стенда засеяна (UWUWU_MOCK_DOCS)"
            );
        }
        Self {
            state: Mutex::new(HubState {
                next_number: 1,
                docs,
                ..HubState::default()
            }),
            seeds,
        }
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut HubState) -> UsecaseResult<T>) -> UsecaseResult<T> {
        let mut guard = self.state.lock().expect("mockhub lock");
        f(&mut guard)
    }

    fn ensure_seed(&self, state: &mut HubState) {
        if !state.tasks.is_empty() {
            return;
        }
        for seed in &self.seeds {
            let mut task = Task::new(seed.number, "мок-цикл конвейера".into(), seed.status);
            // приоритет — свойство строки (носитель решения преемпшена
            // CUP #1259): сид задаёт стартовое значение
            if seed.priority != Priority::Empty {
                task.set_priority(seed.priority);
            }
            // возраст закрытия — вход клина #1293 (сид «N:Done:low:14»)
            if seed.days_closed > 0 {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system clock после эпохи")
                    .as_millis() as u64;
                task.set_closed_at(now_ms - seed.days_closed * 86_400_000);
            }
            state.tasks.insert(seed.number, task);
            if let Some(board) = &seed.board {
                state.board_tags.insert(seed.number, board.clone());
            }
            state
                .bodies
                .insert(seed.number, "## Постановка\n\nМок-E2E стенд.\n".into());
            state.next_number = state.next_number.max(seed.number + 1);
            tracing::info!(
                event = "mockhub.seeded",
                number = seed.number,
                board = seed.board.as_deref().unwrap_or("*"),
                "Seed-строка создана"
            );
        }
    }
}

#[async_trait]
impl TaskBoardRepo for MemoryHub {
    async fn read_board(&self, board: &str) -> UsecaseResult<Vec<Task>> {
        self.with_state(|state| {
            self.ensure_seed(state);
            let mut rows: Vec<Task> = state
                .tasks
                .values()
                // архивная строка покидает снапшот (клин #1293) — как
                // NOT_ARCHIVED-дефолт GitHub items; снесённая (#1158) — тем же
                // свойством: item'а на доске нет
                .filter(|t| !state.archived.contains_key(&t.number()))
                .filter(|t| !state.removed.contains_key(&t.number()))
                .filter(|t| match state.board_tags.get(&t.number()) {
                    Some(tag) => tag == board,
                    None => true,
                })
                .cloned()
                .collect();
            // поля (сессии/ветка/PR) — часть строки: без них тик не видит
            // заявленных сессий и решает спавнить (гэп стенда, ловился как
            // «wakeup не стреляет» — находка e2e ре-энтри); семейные
            // ячейки разворачиваются на оба слота семьи (#1237)
            for t in rows.iter_mut() {
                if let Some(f) = state.fields.get(&t.number()) {
                    t.apply_fields(expand_families(f));
                }
            }
            rows.sort_by_key(|t| t.number());
            Ok(rows)
        })
    }

    async fn set_status(
        &self,
        _board: &str,
        number: u64,
        status: Status,
    ) -> UsecaseResult<uwuwu_usecase::StatusDelivery> {
        // Fault-режим phantom-записей (#1331/#1437): «успех» без записи
        // носителя — статусная ветка на живом хабе ловит это verify'ем
        // (точечный GraphQL-рид, #1437) и отвечает громким отказом из
        // settle; мок стенда воспроизводит наблюдаемый исход — тот же
        // класс отказа, чтобы e2e-фаза 11 проверяла контракт целиком.
        if phantom_writes() {
            return Err(UsecaseError::Board(format!(
                "status #{number}: write \"{}\" not confirmed by fresh carrier read — \
                 not applied (stand phantom mode)",
                uwuwu_usecase::status_name(status)
            )));
        }
        self.with_state(|state| {
            let task = state
                .tasks
                .get_mut(&number)
                .ok_or_else(|| UsecaseError::Board(format!("mockhub: #{number} нет")))?;
            let _ = task.transition(status);
            Ok(())
        })?;
        Ok(uwuwu_usecase::StatusDelivery::Confirmed {
            actual: uwuwu_usecase::status_name(status).to_string(),
        })
    }

    async fn row_status(&self, board: &str, number: u64) -> UsecaseResult<Option<Status>> {
        self.with_state(|state| {
            self.ensure_seed(state);
            Ok(state
                .tasks
                .get(&number)
                .filter(|t| match state.board_tags.get(&t.number()) {
                    Some(tag) => tag == board,
                    None => true,
                })
                .map(|t| {
                    // Терминальная резолюция (#1403, паритет GitHubHub):
                    // закрытый мимо task_close issue терминален независимо
                    // от отставшего поля (UC-07) — гард edit() режет
                    // фантомные переходы громкой канон-ошибкой
                    if state.closed.contains_key(&number) {
                        Status::Done
                    } else {
                        t.status()
                    }
                }))
        })
    }
    async fn row_exists(&self, board: &str, number: u64) -> UsecaseResult<bool> {
        self.with_state(|state| {
            self.ensure_seed(state);
            Ok(state
                .tasks
                .get(&number)
                .filter(|t| match state.board_tags.get(&t.number()) {
                    Some(tag) => tag == board,
                    None => true,
                })
                .is_some())
        })
    }

    async fn set_session(
        &self,
        _board: &str,
        number: u64,
        slot: Slot,
        session: &str,
    ) -> UsecaseResult<()> {
        let s = uwuwu_domain::SessionRef::parse(session)
            .ok_or_else(|| UsecaseError::Board("mockhub: session не ses_…".into()))?;
        self.with_state(|state| {
            // семейная ячейка: Design-спавн пишет в тот же ключ, что и
            // дев-спавн (прод-носитель — одно поле «Сессия dev», #1237)
            state
                .fields
                .entry(number)
                .or_default()
                .set_session(family_cell(slot), s);
            Ok(())
        })
    }

    async fn set_branch(
        &self,
        _board: &str,
        number: u64,
        branch: &uwuwu_domain::BranchName,
    ) -> UsecaseResult<()> {
        self.with_state(|state| {
            state
                .fields
                .entry(number)
                .or_default()
                .set_branch(branch.clone());
            Ok(())
        })
    }

    async fn set_board_cells(
        &self,
        _board: &str,
        number: u64,
        priority: Option<&str>,
        automerge: Option<bool>,
        direct: Option<bool>,
    ) -> UsecaseResult<()> {
        self.with_state(|state| {
            // сид материализуется лениво — симметрично row_status/set_status
            self.ensure_seed(state);
            let task = state
                .tasks
                .get_mut(&number)
                .ok_or_else(|| UsecaseError::Board(format!("mockhub: #{number} нет")))?;
            // ячейки снапшота — часть строки стенда; apply_fields при
            // read_board Empty-приоритет не стирает (домен)
            if let Some(p) = priority {
                task.set_priority(uwuwu_usecase::parse_priority(p));
            }
            if let Some(a) = automerge {
                task.set_automerge(a);
            }
            if let Some(d) = direct {
                task.set_direct(d);
            }
            Ok(())
        })
    }

    async fn clear_session(&self, _board: &str, number: u64, slot: Slot) -> UsecaseResult<()> {
        self.with_state(|state| {
            // clear одной стороны семьи снимает ячейку целиком (одно
            // поле носителя — mirror hub.rs clear_session, #1237)
            state
                .fields
                .entry(number)
                .or_default()
                .clear_session(family_cell(slot));
            Ok(())
        })
    }

    async fn issue_fields(&self, number: u64) -> UsecaseResult<TaskFields> {
        self.with_state(|state| {
            Ok(state
                .fields
                .get(&number)
                .map(expand_families)
                .unwrap_or_default())
        })
    }

    async fn post_comment(&self, number: u64, body: &str) -> UsecaseResult<String> {
        // Fault-режим phantom-записей (#1331): «успех» со стандартным id,
        // состояние носителя не меняется — контрольное чтение юзкейса
        // обязано отказывать громко. Не пустой id: пустой — отдельная
        // fail-open ветка нестандартного ответа.
        if phantom_writes() {
            return Ok("phantom-comment".into());
        }
        self.with_state(|state| {
            let counter = &mut state.counter;
            *counter += 1;
            let id = format!("mockc-{counter}");
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            state.comments.entry(number).or_default().push(CommentRef {
                id: id.clone(),
                created_ms: now,
                body: body.to_string(),
            });
            Ok(id)
        })
    }

    async fn close_issue(&self, number: u64) -> UsecaseResult<()> {
        self.close_issue_ext(number, false).await
    }

    async fn read_issue(&self, _board: &str, number: u64) -> UsecaseResult<IssueView> {
        self.with_state(|state| {
            self.ensure_seed(state);
            let mut task = state
                .tasks
                .get(&number)
                .cloned()
                .ok_or_else(|| UsecaseError::Board(format!("mockhub: #{number} нет")))?;
            // поля (сессии/ветка/PR) — часть строки: рид синхронен
            // read_board (реальный hub несёт поля inline в GET issue);
            // семейные ячейки разворачиваются на оба слота семьи (#1237)
            if let Some(f) = state.fields.get(&number) {
                task.apply_fields(expand_families(f));
            }
            let body = state.bodies.get(&number).cloned().unwrap_or_default();
            let comments = state.comments.get(&number).cloned().unwrap_or_default();
            Ok(IssueView {
                task,
                body,
                comments,
            })
        })
    }

    async fn create_issue(
        &self,
        board: &str,
        _project: &str,
        title: &str,
        body: &str,
        kind_name: Option<&str>,
    ) -> UsecaseResult<u64> {
        self.with_state(|state| {
            let number = state.next_number;
            state.next_number += 1;
            let mut task = Task::new(number, title.to_string(), Status::Analys);
            // POST с type (#1271): имя «принято» — носитель видит парс
            task.set_kind(kind_name.and_then(uwuwu_usecase::parse_task_type));
            state.tasks.insert(number, task);
            state.board_tags.insert(number, board.to_string());
            state.bodies.insert(number, body.to_string());
            Ok(number)
        })
    }

    async fn set_issue_type(&self, number: u64, kind_name: &str) -> UsecaseResult<()> {
        // Fault-режим phantom-записей (#1331): «успех» без записи носителя —
        // контрольное чтение юзкейса обязано отказывать громко (семантика
        // семьи: post_comment/rename_issue)
        if phantom_writes() {
            return Ok(());
        }
        let kind = uwuwu_usecase::parse_task_type(kind_name).ok_or_else(|| {
            UsecaseError::Board(format!("mockhub: тип «{kind_name}» вне словаря issue Type"))
        })?;
        self.with_state(|state| {
            let task = state
                .tasks
                .get_mut(&number)
                .ok_or_else(|| UsecaseError::Board(format!("mockhub: #{number} нет")))?;
            task.set_kind(Some(kind));
            Ok(())
        })
    }

    async fn rename_issue(&self, number: u64, title: &str) -> UsecaseResult<()> {
        if phantom_writes() {
            return Ok(());
        }
        self.with_state(|state| {
            let task = state
                .tasks
                .get_mut(&number)
                .ok_or_else(|| UsecaseError::Board(format!("mockhub: #{number} нет")))?;
            *task = Task::new(number, title.to_string(), task.status());
            Ok(())
        })
    }

    async fn patch_cells(
        &self,
        _number: u64,
        _mutate: uwuwu_usecase::FmMutator,
    ) -> UsecaseResult<()> {
        Ok(())
    }

    async fn close_issue_ext(&self, number: u64, _not_planned: bool) -> UsecaseResult<()> {
        self.with_state(|state| {
            let task = state
                .tasks
                .get_mut(&number)
                .ok_or_else(|| UsecaseError::Board(format!("mockhub: #{number} нет")))?;
            let _ = task.transition(Status::Done);
            state.closed.insert(number, true);
            Ok(())
        })
    }

    async fn archive_row(&self, board: &str, number: u64) -> UsecaseResult<()> {
        self.with_state(|state| {
            let on_board = state.tasks.contains_key(&number)
                && state
                    .board_tags
                    .get(&number)
                    .map(|t| t == board)
                    .unwrap_or(true);
            if !on_board {
                return Err(UsecaseError::Board(format!(
                    "строка #{number} не на доске «{board}»"
                )));
            }
            state.archived.insert(number, true);
            Ok(())
        })
    }

    fn gql_budget_remaining(&self) -> Option<u64> {
        // мок-хаб бюджета не имитирует: гард пропускает
        None
    }

    async fn issues_with_title(&self, title: &str) -> UsecaseResult<Vec<u64>> {
        self.with_state(|state| {
            Ok(state
                .tasks
                .values()
                .filter(|t| t.title() == title)
                .map(|t| t.number())
                .collect())
        })
    }

    async fn replace_body(&self, number: u64, body_md: &str) -> UsecaseResult<()> {
        if phantom_writes() {
            return Ok(());
        }
        self.with_state(|state| {
            if !state.bodies.contains_key(&number) {
                return Err(UsecaseError::Board(format!("mockhub: #{number} нет")));
            }
            state.bodies.insert(number, body_md.to_string());
            Ok(())
        })
    }

    async fn latest_comments(&self, number: u64) -> UsecaseResult<Vec<CommentRef>> {
        self.with_state(|state| Ok(state.comments.get(&number).cloned().unwrap_or_default()))
    }
}

/// Чистка доски (#1158) на стенде: item-id проксируется номером задачи,
/// снос = метка `removed` (строка покидает read_board), undo снимает метку.
#[async_trait]
impl uwuwu_usecase::CleanBoardRepo for MemoryHub {
    async fn fresh_index(
        &self,
        board: &str,
    ) -> UsecaseResult<HashMap<u64, uwuwu_usecase::RowBinding>> {
        self.with_state(|state| {
            self.ensure_seed(state);
            let mut map = HashMap::new();
            for (number, task) in &state.tasks {
                let on_board = !state.archived.contains_key(number)
                    && !state.removed.contains_key(number)
                    && state
                        .board_tags
                        .get(number)
                        .map(|t| t == board)
                        .unwrap_or(true);
                if !on_board {
                    continue;
                }
                let status_raw = uwuwu_usecase::status_name(task.status());
                map.insert(
                    *number,
                    uwuwu_usecase::RowBinding {
                        item_id: *number,
                        status_raw: if status_raw.is_empty() {
                            None
                        } else {
                            Some(status_raw.to_string())
                        },
                    },
                );
            }
            Ok(map)
        })
    }

    async fn fresh_issue(&self, number: u64) -> UsecaseResult<Option<uwuwu_usecase::IssueProbe>> {
        self.with_state(|state| {
            self.ensure_seed(state);
            Ok(state.tasks.get(&number).map(|t| uwuwu_usecase::IssueProbe {
                title: t.title().to_string(),
                // state issue — носитель `closed` (close_issue_ext), не статус
                // колонки: статусная модель доски и state issue независимы
                open: !state.closed.contains_key(&number),
                body: state.bodies.get(&number).cloned().unwrap_or_default(),
            }))
        })
    }

    async fn remove_row(&self, _board: &str, number: u64, item_id: u64) -> UsecaseResult<()> {
        self.with_state(|state| {
            // item-id прокси = номер: чужой id — 404-ветка прод-носителя
            if item_id != number || !state.tasks.contains_key(&number) {
                return Err(UsecaseError::Board(format!(
                    "delete item #{number} (item {item_id}): не применилось (HTTP 404)"
                )));
            }
            state.removed.insert(number, true);
            Ok(())
        })
    }

    async fn restore_row(
        &self,
        _board: &str,
        number: u64,
        status_raw: Option<&str>,
    ) -> UsecaseResult<u64> {
        self.with_state(|state| {
            if !state.tasks.contains_key(&number) {
                return Err(UsecaseError::Board(format!("mockhub: #{number} нет")));
            }
            state.removed.remove(&number);
            if let Some(raw) = status_raw.filter(|s| !s.trim().is_empty()) {
                let status = uwuwu_usecase::parse_status(raw);
                if let Some(task) = state.tasks.get_mut(&number) {
                    let _ = task.transition(status);
                }
            }
            Ok(number)
        })
    }
}

#[async_trait]
impl uwuwu_usecase::WikiRepo for MemoryHub {
    async fn doc_shas(&self) -> UsecaseResult<Vec<(String, String)>> {
        self.with_state(|state| {
            Ok(state
                .docs
                .iter()
                .map(|(slug, md)| {
                    let sha = format!("{:x}", md.len());
                    (slug.clone(), sha)
                })
                .collect())
        })
    }

    async fn read_doc(&self, slug: &str) -> UsecaseResult<Option<String>> {
        self.with_state(|state| Ok(state.docs.get(slug).cloned()))
    }

    async fn list_docs(&self, prefix: &str) -> UsecaseResult<Vec<String>> {
        self.with_state(|state| {
            let norm = prefix.trim_matches('/');
            let mut out: Vec<String> = state
                .docs
                .keys()
                .filter(|p| {
                    norm.is_empty() || p.starts_with(&format!("{norm}/")) || p.as_str() == norm
                })
                .cloned()
                .collect();
            out.sort();
            Ok(out)
        })
    }

    async fn write_doc(&self, slug: &str, content: &str, _message: &str) -> UsecaseResult<()> {
        self.with_state(|state| {
            state.docs.insert(slug.to_string(), content.to_string());
            Ok(())
        })
    }

    async fn append_doc(
        &self,
        slug: &str,
        line: &str,
        _message: &str,
    ) -> UsecaseResult<AppendDocOutcome> {
        self.with_state(|state| {
            let existed = state.docs.contains_key(slug);
            let entry = state.docs.entry(slug.to_string()).or_default();
            if !entry.is_empty() && !entry.ends_with('\n') {
                entry.push('\n');
            }
            entry.push_str(line);
            entry.push('\n');
            Ok(AppendDocOutcome {
                created: !existed,
                lines: entry.lines().count(),
            })
        })
    }
}

/// Fault-режим e2e-фазы phantom-записей (#1331): мутации носителя
/// отвечают успехом, состояние не меняется — контрольное чтение юзкейса
/// обязано превращать «успех» в громкий отказ. Включение: env
/// UWUWU_MOCK_PHANTOM_WRITES=1 (только мок-хаб стенда, прод не касается).
fn phantom_writes() -> bool {
    std::env::var("UWUWU_MOCK_PHANTOM_WRITES").is_ok_and(|v| v == "1")
}

#[async_trait]
impl IssueFieldStore for MemoryHub {
    async fn load(&self, number: u64) -> UsecaseResult<HashMap<String, String>> {
        use uwuwu_domain::Slot;
        self.with_state(|state| {
            let mut out = HashMap::new();
            if let Some(f) = state.fields.get(&number) {
                for (name, slot) in [
                    (crate::github::F_DEV_SESSION, Slot::Dev),
                    (crate::github::F_REVIEW_SESSION, Slot::PlanReview),
                    (crate::github::F_TEST_SESSION, Slot::Test),
                ] {
                    if let Some(s) = f.session(slot) {
                        out.insert(name.into(), s.as_str().to_string());
                    }
                }
                if let Some(b) = f.branch() {
                    out.insert("Branch".into(), b.as_str().to_string());
                }
                if let Some(p) = f.pr() {
                    out.insert("PR".into(), p.as_str().to_string());
                }
            }
            // автомерж — поле строки (#402/#1258: один механизм): наружу
            // нормализованное «Automerge» on/off — гейт #1280 читает здесь.
            // Приоритет — тоже поле строки: имя high/medium/low, Empty
            // не отдаётся (пустота = отсутствие ключа, как в живом fields.rs)
            if let Some(t) = state.tasks.get(&number) {
                out.insert(
                    uwuwu_usecase::F_AUTOMERGE.into(),
                    if t.automerge() { "on" } else { "off" }.into(),
                );
                // direct (#1357): поле строки, как automerge/priority на стенде
                out.insert(
                    uwuwu_usecase::F_DIRECT.into(),
                    if t.direct() { "on" } else { "off" }.into(),
                );
                let name = match t.priority() {
                    uwuwu_domain::Priority::High => Some("high"),
                    uwuwu_domain::Priority::Medium => Some("medium"),
                    uwuwu_domain::Priority::Low => Some("low"),
                    uwuwu_domain::Priority::Empty => None,
                };
                if let Some(n) = name {
                    out.insert(uwuwu_usecase::F_PRIORITY.into(), n.into());
                }
            }
            Ok(out)
        })
    }

    async fn store(&self, number: u64, patch: &[(&str, Option<String>)]) -> UsecaseResult<()> {
        if phantom_writes() {
            return Ok(());
        }
        use uwuwu_domain::Slot;
        self.with_state(|state| {
            if !state.tasks.contains_key(&number) {
                return Err(UsecaseError::Board(format!("mockhub: #{number} нет")));
            }
            // приоритет (#1259 e2e): задача task_edit пишет F_PRIORITY —
            // на стенде значение живёт в самой строке (Task::set_priority)
            if let Some(v) = patch
                .iter()
                .find(|(name, _)| matches!(*name, uwuwu_usecase::F_PRIORITY))
                .and_then(|(_, v)| v.as_deref())
            {
                let task = state.tasks.get_mut(&number).expect("checked above");
                task.set_priority(uwuwu_usecase::parse_priority(v));
            }
            let f = state.fields.entry(number).or_default();
            for (name, value) in patch {
                let slot = match *name {
                    crate::github::F_DEV_SESSION => Some(Slot::Dev),
                    crate::github::F_REVIEW_SESSION => Some(Slot::PlanReview),
                    crate::github::F_TEST_SESSION => Some(Slot::Test),
                    _ => None,
                };
                if let (Some(slot), Some(v)) = (slot, value) {
                    if let Some(s) = uwuwu_domain::SessionRef::parse(v) {
                        f.set_session(slot, s);
                    }
                    continue;
                }
                match (*name, value) {
                    ("Branch", Some(v)) => {
                        if let Some(b) = uwuwu_domain::BranchName::parse(v) {
                            f.set_branch(b);
                        }
                    }
                    ("Automerge" | "Autodone", Some(v)) => {
                        // автомерж-ячейка (#1232): canonical «Automerge»,
                        // legacy «Autodone» — в одну клетку; on/true — вкл.
                        // Хранилище — поле строки (#402/#1258: один механизм,
                        // карта снесена; load нормализует наружу — #1280)
                        if let Some(task) = state.tasks.get_mut(&number) {
                            task.set_automerge(v == "on" || v == "true");
                        }
                    }
                    ("Automerge" | "Autodone", None) => {
                        // очистка по контракту store (None — снять);
                        // в поле строки два состояния — false как default
                        if let Some(task) = state.tasks.get_mut(&number) {
                            task.set_automerge(false);
                        }
                    }
                    ("PR", Some(v)) => {
                        if let Some(p) = uwuwu_domain::PrUrl::parse(v) {
                            // зеркально мапперу носителя (#1155): ветка есть —
                            // кодовый PullRequest, нет — безветочный артефакт
                            if f.branch().is_some() {
                                let _ = f.open_pr(p);
                            } else {
                                f.set_artifact(p);
                            }
                        }
                    }
                    // «Приоритет» — поле строки (#402 set_board_cells):
                    // issue-филд store больше не пишет приоритет, ветка
                    // выпилена (#1280 резолв мержа — мёртвый носитель)
                    _ => {}
                }
            }
            Ok(())
        })
    }
}

#[async_trait]
impl uwuwu_usecase::GoalsRepo for MemoryHub {
    /// E2E-стенд: целей нет — goal_list честно отвечает «не заведены».
    async fn open_goals(&self, _project: &str) -> UsecaseResult<Vec<uwuwu_usecase::Goal>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    //! Пины порта на MemoryHub (#1235): досочный фильтр row_status и
    //! свежесть носителя после записи. Гард канона edit() сверяется с
    //! row_status — семантика носителя (борд-уважение, read-your-writes)
    //! не должна ползти.

    use super::*;
    use crate::api::test_support::StubState;
    use crate::clock::SystemClock;
    use std::sync::Arc;
    use uwuwu_usecase::{TaskPatch, TaskWriteUseCase, TaskWriter};

    /// Расщеплённая строка стенда (#1403, инцидент foilcap #1332): issue
    /// закрыт мимо task_close — close_issue_ext молча блокирует transition
    /// (канон: Done — только через close), поле остаётся «Ревью результата»,
    /// закрытие фиксируется. Живой пример расщепления «поле против state».
    async fn split_row() -> (MemoryHub, u64) {
        let hub = MemoryHub::with_seeds(vec![SeedRow {
            board: Some("Задачи uwuwu".into()),
            number: 21,
            status: Status::ReviewResult,
            priority: Priority::Empty,
            days_closed: 0,
        }]);
        // сид стенда ленивый (ensure_seed на чтениях) — прогрев до close
        assert!(
            hub.row_exists("Задачи uwuwu", 21).await.expect("seed"),
            "сид-строка #21 на доске"
        );
        hub.close_issue_ext(21, false)
            .await
            .expect("закрытие issue мимо task_close");
        (hub, 21)
    }

    #[tokio::test]
    async fn row_status_resolves_closed_issue_over_stale_field() {
        let (hub, n) = split_row().await;
        let current = hub
            .row_status("Задачи uwuwu", n)
            .await
            .expect("чтение носителя");
        assert_eq!(
            current,
            Some(Status::Done),
            "закрытый issue терминален (UC-07) — гард не должен валидировать \
             фантомный статус отставшего поля"
        );
    }

    #[tokio::test]
    async fn edit_on_split_row_is_blocked_with_canon_error() {
        let (hub, n) = split_row().await;
        let hub = Arc::new(hub);
        let writer = TaskWriteUseCase {
            board: hub.clone() as Arc<dyn TaskBoardRepo>,
            fields: hub as Arc<dyn IssueFieldStore>,
            boards: vec![("uwuwu".to_string(), "Задачи uwuwu".to_string())],
            clock: Arc::new(SystemClock),
            state: Arc::new(StubState),
        };
        let patch = TaskPatch {
            status: Some(Status::NeedsAttention),
            ..Default::default()
        };
        let err = writer
            .edit("uwuwu", n, patch)
            .await
            .expect_err("фантомный переход в закрытую строку отклонён гардом");
        assert!(
            format!("{err}").contains("outside the pipeline canon"),
            "громкая канон-ошибка про терминальность, получили: {err}"
        );
    }

    #[tokio::test]
    async fn row_status_respects_board_tag() {
        let hub = MemoryHub::with_seeds(vec![SeedRow {
            board: Some("Задачи uwuwu".into()),
            number: 11,
            status: Status::Analys,
            priority: Priority::Empty,
            days_closed: 0,
        }]);
        let own = hub
            .row_status("Задачи uwuwu", 11)
            .await
            .expect("чтение своей доски");
        assert_eq!(own, Some(Status::Analys));
        // номер на чужой доске — None («строка не на доске»), а не статус
        // чужой доски: код-ревью #1235 Low-1, носители борд-уважающие
        let foreign = hub
            .row_status("Задачи origa", 11)
            .await
            .expect("чтение чужой доски");
        assert_eq!(foreign, None, "строка другой доски — None");
        // без тега — видна на любой доске стенда
        let hub_free = MemoryHub::with_seed(Some((7, Status::InProgress)));
        let any = hub_free
            .row_status("какая-то доска", 7)
            .await
            .expect("чтение без тега");
        assert_eq!(any, Some(Status::InProgress));
    }

    #[tokio::test]
    async fn row_exists_respects_board_tag_and_absent_rows() {
        // Pv2-путь close() (#1236): поиск доски строки — предикат
        // присутствия (строка без читаемого Status всё ещё строка),
        // борд-уважение — как у row_status (#1235 Low-1), мисса — false.
        let hub = MemoryHub::with_seeds(vec![SeedRow {
            board: Some("Задачи uwuwu".into()),
            number: 11,
            status: Status::Analys,
            priority: Priority::Empty,
            days_closed: 0,
        }]);
        assert!(
            hub.row_exists("Задачи uwuwu", 11)
                .await
                .expect("чтение своей доски"),
            "строка своей доски — true"
        );
        assert!(
            !hub.row_exists("Задачи origa", 11)
                .await
                .expect("чтение чужой доски"),
            "номер другой доски — не наша строка"
        );
        assert!(
            !hub.row_exists("Задачи uwuwu", 99).await.expect("мисса"),
            "несуществующая строка — false"
        );
        // строка без тега видна на любой доске стенда (паритет row_status)
        let hub_free = MemoryHub::with_seed(Some((7, Status::InProgress)));
        assert!(
            hub_free
                .row_exists("какая-то доска", 7)
                .await
                .expect("чтение без тега"),
            "строка без тега — на любой доске"
        );
    }

    #[tokio::test]
    async fn set_status_is_visible_to_row_status_immediately() {
        // read-your-writes на носителе: запись статусного пути видна
        // свежему чтению без окна стэйла (#1235). Порядок — как в edit():
        // гард читает row_status ДО set_status (заодно материализует сид).
        let hub = MemoryHub::with_seed(Some((7, Status::Analys)));
        let current = hub.row_status("B", 7).await.expect("чтение до записи");
        assert_eq!(current, Some(Status::Analys));
        hub.set_status("B", 7, Status::ReviewAnalys)
            .await
            .expect("легальное ребро Analys → Review Analys");
        let fresh = hub.row_status("B", 7).await.expect("чтение после записи");
        assert_eq!(
            fresh,
            Some(Status::ReviewAnalys),
            "свежая запись видна сразу"
        );
    }

    #[tokio::test]
    async fn automerge_field_persists_to_rows() {
        // #1155 (поручение владельца): mock-носитель хранит автомерж-ячейку —
        // write-path («Automerge», legacy «Autodone» в одну клетку) →
        // снапшот-риды несут флаг; без этого тик стенда слеп к мерж-гейту
        // (automerge_ready всегда false) и fm врёт «automerge: false».
        // Хранилище — поле строки (#1258: один механизм, карта снесена)
        let hub = MemoryHub::with_seed(Some((9, Status::UserReview)));
        use uwuwu_usecase::IssueFieldStore;
        // сид материализуется лениво (ensure_seed на ридах) — как в пине
        // row_status #1235: чтение до записи
        hub.read_board("B").await.expect("материализация сида");
        hub.store(9, &[(uwuwu_usecase::F_AUTOMERGE, Some("on".to_string()))])
            .await
            .expect("canonical write");
        let rows = hub.read_board("B").await.expect("board");
        assert!(
            rows.iter().find(|t| t.number() == 9).unwrap().automerge(),
            "canonical «Automerge»: on → true на строке снапшота"
        );
        assert!(hub.read_issue("B", 9).await.unwrap().task.automerge());

        hub.store(9, &[("Autodone", Some("off".to_string()))])
            .await
            .expect("legacy write");
        assert!(
            !hub.read_board("B").await.unwrap()[0].automerge(),
            "legacy «Autodone» пишет в ту же ячейку (off)"
        );

        hub.store(9, &[("Automerge", None)]).await.expect("clear");
        assert!(
            !hub.read_board("B").await.unwrap()[0].automerge(),
            "None — очистка по контракту store"
        );
    }

    /// Read-your-writes нестатусных ячеек снапшота (#1258): запись
    /// Приоритета/Automerge видна read_board сразу. До фикса поля шли
    /// мимо стейта (fields.store их терял) — task_list показывал
    /// до-мутационные значения до TTL (проба на проде 18.11).
    #[tokio::test]
    async fn board_cells_are_visible_to_read_board_immediately() {
        let hub = MemoryHub::with_seed(Some((7, Status::Analys)));
        hub.set_board_cells("B", 7, Some("high"), Some(true), None)
            .await
            .expect("запись ячеек");
        let rows = hub.read_board("B").await.expect("чтение доски");
        let t = rows
            .iter()
            .find(|t| t.number() == 7)
            .expect("строка на доске");
        assert_eq!(
            t.priority(),
            uwuwu_domain::Priority::High,
            "приоритет виден сразу"
        );
        assert!(t.automerge(), "automerge виден сразу");
    }

    /// Свежая строка (окно create: add_to_board инвалидирует снапшот ДО
    /// записи полей) — записанные поля приземляются в read_board (#1258).
    #[tokio::test]
    async fn created_row_board_cells_landed_in_read_board() {
        let hub = MemoryHub::with_seed(None);
        let n = hub
            .create_issue("B", "uwuwu", "строка", "тело", Some("Refactoring"))
            .await
            .expect("создание строки");
        hub.set_board_cells("B", n, Some("low"), Some(false), None)
            .await
            .expect("запись ячеек свежей строки");
        let rows = hub.read_board("B").await.expect("чтение доски");
        let t = rows
            .iter()
            .find(|t| t.number() == n)
            .expect("строка на доске");
        assert_eq!(t.priority(), uwuwu_domain::Priority::Low);
        assert_eq!(
            t.kind(),
            Some(uwuwu_domain::TaskType::Refactoring),
            "#1271: тип из POST create виден в read_board стенда"
        );
        assert!(!t.automerge());
    }

    #[tokio::test]
    async fn seeded_priority_reaches_read_board() {
        // Носитель приоритета на стенде (преемпшен CUP #1259): сид с
        // хвостом «:priority» виден тику как приоритет строки доски
        let hub = MemoryHub::with_seeds(parse_seeds(
            "2046:In Progress:High;2048:In Progress:low;2049:Design",
        ));
        let rows = hub.read_board("Задачи uwuwu").await.expect("доска");
        let prio = |n: u64| rows.iter().find(|t| t.number() == n).map(|t| t.priority());
        assert_eq!(prio(2046), Some(Priority::High), "сид-приоритет High");
        assert_eq!(prio(2048), Some(Priority::Low), "регистр не влияет");
        assert_eq!(
            prio(2049),
            Some(Priority::Empty),
            "сид без хвоста — дефолт Empty"
        );
    }

    #[tokio::test]
    async fn task_edit_priority_updates_row() {
        // Понижение приоритета активной строки (#1259 e2e): task_edit
        // пишет F_PRIORITY через IssueFieldStore — строка меняет приоритет
        // (can_preempt живёт в строке доски, не в полях-носителях)
        let hub = MemoryHub::with_seeds(parse_seeds("2046:In Progress:High"));
        // материализация сида (в проде edit() гардит row_status ДО store)
        hub.read_board("Задачи uwuwu").await.expect("доска");
        hub.store(2046, &[(uwuwu_usecase::F_PRIORITY, Some("low".into()))])
            .await
            .expect("запись приоритета");
        let rows = hub.read_board("Задачи uwuwu").await.expect("доска");
        let prio = rows
            .iter()
            .find(|t| t.number() == 2046)
            .map(|t| t.priority());
        assert_eq!(
            prio,
            Some(Priority::Low),
            "task_edit-приоритет виден строке"
        );
    }

    #[tokio::test]
    async fn archived_row_leaves_board_but_stays_readable() {
        // Холодный ящик (#1293, сценарий «Проверки»): архивная строка
        // покидает read_board (снапшот сжат), но знание не теряется —
        // issue-first чтение (task_get) и статус доступны. Регресс до
        // archived-фильтра в read_issue/row_status краснит вторую половину.
        let hub = MemoryHub::with_seeds(parse_seeds("Задачи uwuwu:2049:Done:low:14"));
        let before = hub.read_board("Задачи uwuwu").await.expect("доска до");
        assert!(
            before.iter().any(|t| t.number() == 2049),
            "свежий Done-сид на доске до архивации"
        );
        hub.archive_row("Задачи uwuwu", 2049)
            .await
            .expect("архивация терминальной строки");
        let after = hub.read_board("Задачи uwuwu").await.expect("доска после");
        assert!(
            !after.iter().any(|t| t.number() == 2049),
            "архивная строка покинула снапшот"
        );
        let view = hub
            .read_issue("Задачи uwuwu", 2049)
            .await
            .expect("issue-first чтение архивной строки (task_get холодный путь)");
        assert_eq!(
            view.task.number(),
            2049,
            "тело/мета строки доступны после архива"
        );
        let status = hub
            .row_status("Задачи uwuwu", 2049)
            .await
            .expect("статус архивной строки");
        assert_eq!(status, Some(Status::Done), "статус не потерян");
    }

    /// Семейная семантика носителя стенда (#1237, mirror github/hub.rs):
    /// Design-спавн пишет в канонический ключ семьи («Сессия dev» = Dev),
    /// чтение разворачивает ячейку на оба слота — дев-сессия «переезжает»
    /// из Design в In Progress без нового спавна (прод-геометрия wake).
    /// До фикса: per-slot запись давала пустой дев-слот — тик спавнил
    /// свежего дева, wake-путь без адресата.
    #[tokio::test]
    async fn design_session_reads_in_dev_slot_after_transition() {
        let hub = MemoryHub::with_seeds(parse_seeds("2049:Design"));
        hub.read_board("Задачи uwuwu")
            .await
            .expect("материализация сида");
        hub.set_session("Задачи uwuwu", 2049, Slot::Design, "ses_design1")
            .await
            .expect("запись design-сессии");
        let rows = hub.read_board("Задачи uwuwu").await.expect("доска");
        let t = rows.iter().find(|t| t.number() == 2049).expect("строка");
        assert_eq!(
            t.session_of(Slot::Design),
            Some("ses_design1"),
            "Design-слот видит сессию"
        );
        assert_eq!(
            t.session_of(Slot::Dev),
            Some("ses_design1"),
            "семейная ячейка: та же сессия в дев-слоте — переход Design → In Progress не спавнит нового дева"
        );
    }

    /// Design-запись surfaceится в IssueFieldStore::load как «Сессия dev»
    /// (#1237): одна ячейка семьи — MCP-чтение полей согласовано с тиком
    /// (до фикса load был слеп к Design-ключу).
    #[tokio::test]
    async fn fields_load_surfaces_design_session_as_dev_field() {
        let hub = MemoryHub::with_seeds(parse_seeds("2049:Design"));
        hub.read_board("Задачи uwuwu")
            .await
            .expect("материализация сида");
        hub.set_session("Задачи uwuwu", 2049, Slot::Design, "ses_design1")
            .await
            .expect("запись design-сессии");
        let fields = uwuwu_usecase::IssueFieldStore::load(&hub, 2049)
            .await
            .expect("поля");
        assert_eq!(
            fields.get(crate::github::F_DEV_SESSION).map(String::as_str),
            Some("ses_design1"),
            "Design-спавн виден как «Сессия dev»"
        );
    }

    /// Clear одной стороны семьи снимает ячейку целиком (#1237): одно
    /// поле носителя — clear_session(Dev) и clear_session(Design) равнозначны.
    #[tokio::test]
    async fn clear_session_removes_family_cell_from_both_sides() {
        let hub = MemoryHub::with_seeds(parse_seeds("2049:Design"));
        hub.read_board("Задачи uwuwu")
            .await
            .expect("материализация сида");
        hub.set_session("Задачи uwuwu", 2049, Slot::Design, "ses_design1")
            .await
            .expect("запись design-сессии");
        hub.clear_session("Задачи uwuwu", 2049, Slot::Dev)
            .await
            .expect("clear дев-слотом");
        let rows = hub.read_board("Задачи uwuwu").await.expect("доска");
        let t = rows.iter().find(|t| t.number() == 2049).expect("строка");
        assert_eq!(t.session_of(Slot::Design), None, "ячейка снята для Design");
        assert_eq!(t.session_of(Slot::Dev), None, "ячейка снята для Dev");
        // симметрично: запись дев-слотом, clear design-слотом
        hub.set_session("Задачи uwuwu", 2049, Slot::Dev, "ses_dev2")
            .await
            .expect("запись дев-сессии");
        hub.clear_session("Задачи uwuwu", 2049, Slot::Design)
            .await
            .expect("clear design-слотом");
        let rows = hub.read_board("Задачи uwuwu").await.expect("доска");
        let t = rows.iter().find(|t| t.number() == 2049).expect("строка");
        assert_eq!(t.session_of(Slot::Design), None);
        assert_eq!(t.session_of(Slot::Dev), None);
    }

    /// Ревью-семья — та же схема (#1237): «Сессия ревью» покрывает
    /// PlanReview|ResultReview, clear одной стороны снимает обе.
    #[tokio::test]
    async fn review_family_cell_shared_across_review_slots() {
        let hub = MemoryHub::with_seed(Some((9, Status::ReviewPlan)));
        hub.read_board("B").await.expect("материализация сида");
        hub.set_session("B", 9, Slot::PlanReview, "ses_review1")
            .await
            .expect("запись ревью-сессии");
        let rows = hub.read_board("B").await.expect("доска");
        let t = rows.iter().find(|t| t.number() == 9).expect("строка");
        assert_eq!(t.session_of(Slot::PlanReview), Some("ses_review1"));
        assert_eq!(
            t.session_of(Slot::ResultReview),
            Some("ses_review1"),
            "ревью-семья: одна ячейка на оба ревью-слота"
        );
        hub.clear_session("B", 9, Slot::ResultReview)
            .await
            .expect("clear result-слотом");
        let rows = hub.read_board("B").await.expect("доска");
        let t = rows.iter().find(|t| t.number() == 9).expect("строка");
        assert_eq!(
            t.session_of(Slot::PlanReview),
            None,
            "clear одной стороны снимает семейную ячейку"
        );
        assert_eq!(t.session_of(Slot::ResultReview), None);
    }
}
