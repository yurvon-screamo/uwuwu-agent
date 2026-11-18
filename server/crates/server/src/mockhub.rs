//! Мок-hub: доска/issues/поля в памяти (E2E-стенд без GitHub и rate-limit).
//! Включается конфигом [hub] driver = "memory". Совместим с остальным
//! контуром: тик, MCP-инструменты, мок-агенты — всё через порты.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;
use uwuwu_domain::{Priority, Slot, Status, Task, TaskFields};
use uwuwu_usecase::{CommentRef, IssueView};
use uwuwu_usecase::{IssueFieldStore, TaskBoardRepo};
use uwuwu_usecase::{UsecaseError, UsecaseResult};

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
    counter: u64,
    /// Слэг → markdown (вики стенда; seed UWUWU_MOCK_DOCS — JSON-карта).
    docs: HashMap<String, String>,
}

/// Seed-строка стенда: доска (опционально), номер, стартовый статус,
/// приоритет (опционально — дефолт Empty; носитель приоритета для
/// преемпшена CUP #1259, чьё решение живёт в строке доски).
struct SeedRow {
    board: Option<String>,
    number: u64,
    status: Status,
    priority: Priority,
}

/// Парсинг UWUWU_MOCK_SEED: «;»-раздельные строки «board:number:status»
/// или «number:status» (без тега — видна на всех досках стенда);
/// хвост «:high|medium|low» задаёт приоритет (преемпшен #1259 e2e).
/// Трёхчастные сиды дискриминируются первой частью: число →
/// «number:status:priority», иначе — «board:number:status» (доска
/// нечисловая; «In Progress» номером быть не может — парсер отсечёт).
fn parse_seeds(raw: &str) -> Vec<SeedRow> {
    raw.split(';')
        .filter_map(|entry| {
            let parts: Vec<&str> = entry.split(':').collect();
            let (board, number, status, prio) = match parts.as_slice() {
                [board, n, s, p] => (Some((*board).trim().to_string()), n, s, *p),
                [a, b, c] if a.trim().parse::<u64>().is_ok() => (None, a, b, *c),
                // прочие трёхчастные — «board:number:status» (без тега с
                // приоритетом — 4-частная форма «board:number:status:pr»)
                [board, n, s] => (Some((*board).trim().to_string()), n, s, ""),
                [n, s] => (None, n, s, ""),
                _ => return None,
            };
            Some(SeedRow {
                board,
                number: number.trim().parse().ok()?,
                status: uwuwu_usecase::parse_status(status.trim()),
                priority: uwuwu_usecase::parse_priority(prio),
            })
        })
        .collect()
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
                .filter(|t| match state.board_tags.get(&t.number()) {
                    Some(tag) => tag == board,
                    None => true,
                })
                .cloned()
                .collect();
            // поля (сессии/ветка/PR) — часть строки: без них тик не видит
            // заявленных сессий и решает спавнить (гэп стенда, ловился как
            // «wakeup не стреляет» — находка e2e ре-энтри)
            for t in rows.iter_mut() {
                if let Some(f) = state.fields.get(&t.number()) {
                    t.apply_fields(f.clone());
                }
            }
            rows.sort_by_key(|t| t.number());
            Ok(rows)
        })
    }

    async fn set_status(&self, _board: &str, number: u64, status: Status) -> UsecaseResult<()> {
        self.with_state(|state| {
            let task = state
                .tasks
                .get_mut(&number)
                .ok_or_else(|| UsecaseError::Board(format!("mockhub: #{number} нет")))?;
            let _ = task.transition(status);
            Ok(())
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
                .map(|t| t.status()))
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
            state.fields.entry(number).or_default().set_session(slot, s);
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
            Ok(())
        })
    }

    async fn clear_session(&self, _board: &str, number: u64, slot: Slot) -> UsecaseResult<()> {
        self.with_state(|state| {
            state.fields.entry(number).or_default().clear_session(slot);
            Ok(())
        })
    }

    async fn issue_fields(&self, number: u64) -> UsecaseResult<TaskFields> {
        self.with_state(|state| Ok(state.fields.get(&number).cloned().unwrap_or_default()))
    }

    async fn post_comment(&self, number: u64, body: &str) -> UsecaseResult<String> {
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
            // read_board (реальный hub несёт поля inline в GET issue)
            if let Some(f) = state.fields.get(&number) {
                task.apply_fields(f.clone());
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
    ) -> UsecaseResult<u64> {
        self.with_state(|state| {
            let number = state.next_number;
            state.next_number += 1;
            state
                .tasks
                .insert(number, Task::new(number, title.to_string(), Status::Analys));
            state.board_tags.insert(number, board.to_string());
            state.bodies.insert(number, body.to_string());
            Ok(number)
        })
    }

    async fn rename_issue(&self, number: u64, title: &str) -> UsecaseResult<()> {
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

    async fn replace_body(&self, number: u64, body_md: &str) -> UsecaseResult<()> {
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

    async fn append_doc(&self, slug: &str, line: &str, _message: &str) -> UsecaseResult<()> {
        self.with_state(|state| {
            let entry = state.docs.entry(slug.to_string()).or_default();
            if !entry.is_empty() {
                entry.push('\n');
            }
            entry.push_str(line);
            Ok(())
        })
    }
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

#[cfg(test)]
mod tests {
    //! Пины порта на MemoryHub (#1235): досочный фильтр row_status и
    //! свежесть носителя после записи. Гард канона edit() сверяется с
    //! row_status — семантика носителя (борд-уважение, read-your-writes)
    //! не должна ползти.

    use super::*;

    #[tokio::test]
    async fn row_status_respects_board_tag() {
        let hub = MemoryHub::with_seeds(vec![SeedRow {
            board: Some("Задачи uwuwu".into()),
            number: 11,
            status: Status::Analys,
            priority: Priority::Empty,
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
        hub.set_board_cells("B", 7, Some("high"), Some(true))
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
            .create_issue("B", "uwuwu", "строка", "тело")
            .await
            .expect("создание строки");
        hub.set_board_cells("B", n, Some("low"), Some(false))
            .await
            .expect("запись ячеек свежей строки");
        let rows = hub.read_board("B").await.expect("чтение доски");
        let t = rows
            .iter()
            .find(|t| t.number() == n)
            .expect("строка на доске");
        assert_eq!(t.priority(), uwuwu_domain::Priority::Low);
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
}
