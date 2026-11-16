//! Мок-hub: доска/issues/поля в памяти (E2E-стенд без GitHub и rate-limit).
//! Включается конфигом [hub] driver = "memory". Совместим с остальным
//! контуром: тик, MCP-инструменты, мок-агенты — всё через порты.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;
use uwuwu_domain::{Slot, Status, Task, TaskFields};
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
    /// number → поля (сессии/ветка/PR)
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

/// Seed-строка стенда: доска (опционально), номер, стартовый статус.
struct SeedRow {
    board: Option<String>,
    number: u64,
    status: Status,
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
            .map(|raw| {
                raw.split(';')
                    .filter_map(|entry| {
                        let parts: Vec<&str> = entry.split(':').collect();
                        match parts.as_slice() {
                            [board, n, s] => Some(SeedRow {
                                board: Some(board.trim().to_string()),
                                number: n.trim().parse().ok()?,
                                status: uwuwu_usecase::parse_status(s.trim()),
                            }),
                            [n, s] => Some(SeedRow {
                                board: None,
                                number: n.trim().parse().ok()?,
                                status: uwuwu_usecase::parse_status(s.trim()),
                            }),
                            _ => None,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
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
            state.tasks.insert(
                seed.number,
                Task::new(seed.number, "мок-цикл конвейера".into(), seed.status),
            );
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

    async fn clear_session(&self, _board: &str, number: u64, slot: Slot) -> UsecaseResult<()> {
        self.with_state(|state| {
            state.fields.entry(number).or_default().clear_session(slot);
            Ok(())
        })
    }

    async fn issue_fields(&self, number: u64) -> UsecaseResult<TaskFields> {
        self.with_state(|state| Ok(state.fields.get(&number).cloned().unwrap_or_default()))
    }

    async fn post_comment(&self, number: u64, body: &str) -> UsecaseResult<()> {
        self.with_state(|state| {
            let counter = &mut state.counter;
            *counter += 1;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            state.comments.entry(number).or_default().push(CommentRef {
                id: format!("mockc-{counter}"),
                created_ms: now,
                body: body.to_string(),
            });
            Ok(())
        })
    }

    async fn close_issue(&self, number: u64) -> UsecaseResult<()> {
        self.close_issue_ext(number, false).await
    }

    async fn read_issue(&self, _board: &str, number: u64) -> UsecaseResult<IssueView> {
        self.with_state(|state| {
            self.ensure_seed(state);
            let task = state
                .tasks
                .get(&number)
                .cloned()
                .ok_or_else(|| UsecaseError::Board(format!("mockhub: #{number} нет")))?;
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
                .insert(number, Task::new(number, title.to_string(), Status::Draft));
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
            Ok(out)
        })
    }

    async fn store(&self, number: u64, patch: &[(&str, Option<String>)]) -> UsecaseResult<()> {
        use uwuwu_domain::Slot;
        self.with_state(|state| {
            if !state.tasks.contains_key(&number) {
                return Err(UsecaseError::Board(format!("mockhub: #{number} нет")));
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
                    ("PR", Some(v)) => {
                        if let Some(p) = uwuwu_domain::PrUrl::parse(v) {
                            let _ = f.open_pr(p);
                        }
                    }
                    _ => {}
                }
            }
            Ok(())
        })
    }
}
