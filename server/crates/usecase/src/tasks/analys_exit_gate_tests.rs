//! Гейт полноты обязательных полей на выходе из Analys (#1280): волна
//! аналитики обязана выпускать строки с явными Приоритетом и Automerge —
//! решение разбора, не пустота носителя. Матрица: пустое поле → отказ
//! с перечнем; оба заполнены или перекрыты патчем → пропуск; не-Analys
//! и Deleted → без проверки; идемпотентный Analys → Analys под гейтом
//! (can_transition пропускает from == to); мусорный приоритет = пустота;
//! отказ атомарен — set_status не вызывается.

use crate::error::UsecaseResult;
use crate::ports::{CommentRef, FmMutator, IssueFieldStore, IssueView, TaskBoardRepo};
use crate::tasks::write::{TaskPatch, TaskWriteUseCase, TaskWriter};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uwuwu_domain::{BranchName, Slot, Status, Task, TaskFields};

/// Носитель статусов + журнал записей set_status: отказ гейта обязан
/// не писать (атомарность видна в тесте, не на словах).
struct GateBoard {
    statuses: Mutex<HashMap<u64, Status>>,
    writes: Mutex<Vec<(u64, Status)>>,
}

impl GateBoard {
    fn with_status(number: u64, status: Status) -> Self {
        Self {
            statuses: Mutex::new(HashMap::from([(number, status)])),
            writes: Mutex::new(Vec::new()),
        }
    }

    fn writes(&self) -> Vec<(u64, Status)> {
        self.writes.lock().expect("gate board").clone()
    }
}

#[async_trait]
impl TaskBoardRepo for GateBoard {
    async fn read_board(&self, _board: &str) -> UsecaseResult<Vec<Task>> {
        Ok(Vec::new())
    }

    async fn set_status(&self, _b: &str, n: u64, s: Status) -> UsecaseResult<()> {
        self.writes.lock().expect("gate board").push((n, s));
        self.statuses.lock().expect("gate board").insert(n, s);
        Ok(())
    }

    async fn row_status(&self, _b: &str, n: u64) -> UsecaseResult<Option<Status>> {
        Ok(self.statuses.lock().expect("gate board").get(&n).copied())
    }

    async fn set_session(&self, _: &str, _: u64, _: Slot, _: &str) -> UsecaseResult<()> {
        Ok(())
    }

    async fn set_branch(&self, _: &str, _: u64, _: &BranchName) -> UsecaseResult<()> {
        Ok(())
    }

    async fn clear_session(&self, _: &str, _: u64, _: Slot) -> UsecaseResult<()> {
        Ok(())
    }

    async fn post_comment(&self, _: u64, _: &str) -> UsecaseResult<String> {
        Ok(String::new())
    }

    async fn close_issue(&self, _: u64) -> UsecaseResult<()> {
        Ok(())
    }

    async fn latest_comments(&self, _: u64) -> UsecaseResult<Vec<CommentRef>> {
        Ok(Vec::new())
    }

    async fn read_issue(&self, _b: &str, n: u64) -> UsecaseResult<IssueView> {
        Ok(IssueView {
            task: Task::new(n, format!("строка {n}"), Status::Todo),
            body: String::new(),
            comments: vec![],
        })
    }

    async fn issue_fields(&self, _: u64) -> UsecaseResult<TaskFields> {
        Ok(TaskFields::default())
    }

    async fn create_issue(&self, _: &str, _: &str, _: &str, _: &str) -> UsecaseResult<u64> {
        Ok(1)
    }

    async fn rename_issue(&self, _: u64, _: &str) -> UsecaseResult<()> {
        Ok(())
    }

    async fn patch_cells(&self, _: u64, _: FmMutator) -> UsecaseResult<()> {
        Ok(())
    }

    async fn close_issue_ext(&self, _: u64, _: bool) -> UsecaseResult<()> {
        Ok(())
    }

    async fn replace_body(&self, _: u64, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
}

/// Поля issue на носителе: карта непустых значений (load = снятый клон).
#[derive(Default)]
struct GateFields {
    stored: Mutex<HashMap<String, String>>,
}

impl GateFields {
    fn with(entries: &[(&str, &str)]) -> Self {
        Self {
            stored: Mutex::new(entries
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect()),
        }
    }
}

#[async_trait]
impl IssueFieldStore for GateFields {
    async fn load(&self, _n: u64) -> UsecaseResult<HashMap<String, String>> {
        Ok(self.stored.lock().expect("gate fields").clone())
    }

    async fn store(&self, _n: u64, patch: &[(&str, Option<String>)]) -> UsecaseResult<()> {
        let mut map = self.stored.lock().expect("gate fields");
        for (name, value) in patch {
            match value {
                Some(v) if !v.is_empty() => {
                    map.insert((*name).into(), v.clone());
                }
                _ => {
                    map.remove(*name);
                }
            }
        }
        Ok(())
    }
}

/// Writer над фейками + Arc-клоны для проверок носителя и журнала.
fn case(
    status: Status,
    fields: &[(&str, &str)],
) -> (TaskWriteUseCase, Arc<GateBoard>, Arc<GateFields>) {
    let board = Arc::new(GateBoard::with_status(1, status));
    let fields = Arc::new(GateFields::with(fields));
    let u = TaskWriteUseCase {
        board: board.clone(),
        fields: fields.clone(),
        boards: vec![("uwuwu".into(), "Задачи uwuwu".into())],
        clock: Arc::new(crate::pipeline::tick_tests::FixedClock),
        state: Arc::new(crate::pipeline::tick_tests::MemState::empty()),
    };
    (u, board, fields)
}

fn status_patch(status: Status) -> TaskPatch {
    TaskPatch {
        status: Some(status),
        ..TaskPatch::default()
    }
}

/// Пустой Приоритет на носителе → отказ, перечень называет поле,
/// заполненный Automerge в перечне не звать.
#[tokio::test]
async fn analys_exit_requires_priority() {
    let (u, _, _) = case(Status::Analys, &[("Automerge", "on")]);
    let err = u
        .edit("uwuwu", 1, status_patch(Status::ReviewAnalys))
        .await
        .expect_err("пустой Приоритет должен отказать");
    let text = err.to_string();
    assert!(text.contains("Приоритет"), "перечень без Приоритета: {text}");
    assert!(
        !text.contains("Automerge"),
        "перечень зовёт заполненное поле: {text}"
    );
}

/// Пустой Automerge → отказ; заполненный Приоритет не в перечне.
#[tokio::test]
async fn analys_exit_requires_automerge() {
    let (u, _, _) = case(Status::Analys, &[("Приоритет", "high")]);
    let err = u
        .edit("uwuwu", 1, status_patch(Status::ReviewAnalys))
        .await
        .expect_err("пустой Automerge должен отказать");
    let text = err.to_string();
    assert!(text.contains("Automerge"), "перечень без Automerge: {text}");
    assert!(!text.contains("Приоритет"), "заполненное поле в перечне: {text}");
}

/// Оба поля пусты → перечень называет оба.
#[tokio::test]
async fn analys_exit_lists_both_empty_fields() {
    let (u, _, _) = case(Status::Analys, &[]);
    let err = u
        .edit("uwuwu", 1, status_patch(Status::ReviewAnalys))
        .await
        .expect_err("оба пусты — должен отказать");
    let text = err.to_string();
    assert!(
        text.contains("Приоритет") && text.contains("Automerge"),
        "перечень неполный: {text}"
    );
}

/// Оба поля на носителе → переход проходит, статус записан.
#[tokio::test]
async fn analys_exit_passes_with_carrier_values() {
    let (u, board, _) = case(
        Status::Analys,
        &[("Приоритет", "medium"), ("Automerge", "on")],
    );
    u.edit("uwuwu", 1, status_patch(Status::ReviewAnalys))
        .await
        .expect("носитель полон — переход легален");
    assert_eq!(
        board.writes(),
        vec![(1, Status::ReviewAnalys)],
        "статус не записан"
    );
}

/// Патч перекрывает пустой носитель: status+priority+automerge одним
/// вызовом — легально (одно-вызовное дозаполнение с переходом).
#[tokio::test]
async fn analys_exit_passes_when_patch_carries_values() {
    let (u, _, _) = case(Status::Analys, &[]);
    let mut p = status_patch(Status::ReviewAnalys);
    p.priority = Some("high".into());
    p.automerge = Some(true);
    u.edit("uwuwu", 1, p).await.expect("патч несёт оба значения — пропуск");
}

/// Переход не из Analys гейт не трогает (поля пусты, переход легален).
#[tokio::test]
async fn non_analys_transition_is_not_gated() {
    let (u, _, _) = case(Status::Todo, &[]);
    u.edit("uwuwu", 1, status_patch(Status::Design))
        .await
        .expect("вне Analys гейта нет");
}

/// Снос (Analys → Deleted) мимо гейта: дедуп не обязан заполнять поля дублю.
#[tokio::test]
async fn analys_delete_is_not_gated() {
    let (u, _, _) = case(Status::Analys, &[]);
    u.edit("uwuwu", 1, status_patch(Status::Deleted))
        .await
        .expect("Deleted — без проверки полей");
}

/// Идемпотентная переустановка Analys → Analys (from == to) под гейтом:
/// can_transition пропускает ребро, полнота полей проверяется так же.
#[tokio::test]
async fn analys_idempotent_transition_is_gated() {
    let (u, _, _) = case(Status::Analys, &[]);
    let err = u
        .edit("uwuwu", 1, status_patch(Status::Analys))
        .await
        .expect_err("переустановка той же стадии — тоже выход под гейт");
    assert!(
        err.to_string().contains("Analys"),
        "не тот отказ: {err}"
    );
}

/// Мусорный приоритет («urgent» — не high/medium/low) считается пустотой:
/// явное решение — распознанное значение, не произвольная строка.
#[tokio::test]
async fn garbage_priority_is_rejected() {
    let (u, _, _) = case(
        Status::Analys,
        &[("Приоритет", "urgent"), ("Automerge", "on")],
    );
    let err = u
        .edit("uwuwu", 1, status_patch(Status::ReviewAnalys))
        .await
        .expect_err("мусорный приоритет — не явное решение");
    assert!(
        err.to_string().contains("Приоритет"),
        "мусор должен попасть в перечень: {err}"
    );
}

/// Атомарность: отказ гейта не оставляет записи статуса (журнал пуст).
#[tokio::test]
async fn gate_refusal_writes_nothing() {
    let (u, board, _) = case(Status::Analys, &[]);
    let mut p = status_patch(Status::ReviewAnalys);
    p.automerge = Some(true);
    let _ = u.edit("uwuwu", 1, p).await;
    assert!(
        board.writes().is_empty(),
        "отказ записал статус: {:?}",
        board.writes()
    );
}
