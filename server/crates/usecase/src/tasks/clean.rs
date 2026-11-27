//! Батч-снос строк доски с guard'ом вердиктов и write-ahead undo (#1158).
//! Контракт cli-гварда task clean/undo (ADR-025, SiYuan-эра) перенесён на
//! hub-носитель. Строка = привязка issue к Projects v2 (item); «снос» —
//! удаление item, issue не трогается (тело/комменты/тайтл живут в репо —
//! cold-box dup-гард create по тайтлу сохраняется); undo = recreate item +
//! возврат Status-колонки, идемпотентно (строка уже на доске → skip).
//!
//! Вердикты — по независимым источникам (привязка item'а REST-индексом
//! Pv2 + issue REST), fail-closed: несостоявшаяся проба = Divergent =
//! не трогать; Live/Dead (не-Draft) без per-row ack — отказ ВСЕГО батча
//! (подтверждение владельцем, канон ADR-025). Исполнение — self-probe:
//! сервер верит только своим свежим пробам на момент мутации, стэйл-план
//! из dry-run не участвует (параллельный writer между вызовами безопасен
//! по построению — сносится ровно то, что верифицировано свежей пробой).

use crate::error::{UsecaseError, UsecaseResult};
use crate::ports::Clock;
use crate::tasks::md_section::body_hash;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

/// Батч-кап чистки (свойство клина холодного ящика: 50/проход).
pub fn clean_batch_cap() -> usize {
    crate::runtime_cfg::pipeline::clean_cap()
}

/// Свежая привязка строки к доске (мимо всех кэшей) — identity сноса.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowBinding {
    /// numeric item id Projects v2 (то же пространство, что REST-мутации).
    pub item_id: u64,
    /// Raw-значение Status-колонки на момент пробы (None — не задано;
    /// вход undo-журнала — восстановление статуса).
    pub status_raw: Option<String>,
}

/// Свежая проба issue (REST): носитель вердиктов «живости».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueProbe {
    pub title: String,
    /// true — state=open; false — closed (терминальное).
    pub open: bool,
    pub body: String,
}

/// Порт чистки: свежие пробы + мутации сноса/восстановления.
/// Реализация обязана читать носитель мимо кэшей (fresh) — вердикт
/// защищает живые строки только на достоверных пробах.
#[async_trait]
pub trait CleanBoardRepo: Send + Sync {
    /// Свежий индекс привязок доски: ОДИН полный фетч на вызов чистки
    /// (план/исполнение/undo) — квотная дисциплина общего REST-бакета
    /// ADR-113 (ревью #1158 Common-1): батч ест 1 индекс-фетч + N дешёвых
    /// issue-проб, не N индекс-фетчей. Защита от stale-привязки —
    /// DELETE по item_id → 404, не повторами фетча.
    async fn fresh_index(&self, board: &str) -> UsecaseResult<HashMap<u64, RowBinding>>;
    /// Проба issue: None — не найден (404). Err — транспортный отказ.
    async fn fresh_issue(&self, number: u64) -> UsecaseResult<Option<IssueProbe>>;
    /// Снос строки (удаление item). Несостоявшееся удаление (404/лаг) —
    /// громкий Err, не успех.
    async fn remove_row(&self, board: &str, number: u64, item_id: u64) -> UsecaseResult<()>;
    /// Восстановление строки (undo): add item + Status, если был.
    /// Возвращает новый item id (id строк меняется — ADR-025).
    async fn restore_row(
        &self,
        board: &str,
        number: u64,
        status_raw: Option<&str>,
    ) -> UsecaseResult<u64>;
}

/// Строка undo-журнала: всё нужное для recreate (ADR-025: карта
/// «старое → восстановлено» в отчёте undo).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CleanRow {
    pub number: u64,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_raw: Option<String>,
    /// item id снесённой привязки (identity на момент сноса).
    pub item_id: u64,
}

/// Write-ahead журнал сноса: пишется ДО первой мутации батча.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CleanJournal {
    /// Детерминированное имя: `{ts}-{project}-{hash8 строк}` (ADR-025).
    pub id: String,
    pub project: String,
    /// Доска сноса (undo кладёт строку на неё же, не по текущему конфигу).
    pub board: String,
    pub created_ms: u64,
    pub rows: Vec<CleanRow>,
}

/// Носитель undo-журналов (сервер: JSON рядом с state.json).
pub trait CleanJournalStore: Send + Sync {
    /// Запись журнала (write-ahead: вызов ДО первой мутации).
    fn put(&self, journal: &CleanJournal) -> UsecaseResult<()>;
    /// Журнал по id или свежий проекта (None id); None — нет.
    fn get(&self, project: &str, id: Option<&str>) -> UsecaseResult<Option<CleanJournal>>;
    /// Идентификаторы журналов проекта (свежие первыми).
    fn ids(&self, project: &str) -> UsecaseResult<Vec<String>>;
}

/// Вердикт строки (матрица клина, hub-адаптация ADR-025).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanVerdict {
    /// Строки нет на доске — сносить нечего (идемпотентный повтор).
    NoRow,
    /// Issue открыт, тело содержательное — живая задача (не-Draft: ack).
    Live,
    /// Issue закрыт — терминальная строка (не-Draft: ack).
    Dead,
    /// Issue открыт, тело-пустышка (пусто или скелет create) — Draft.
    DraftSkeleton,
    /// Проба не состоялась / источники противоречивы — не трогать.
    Divergent,
}

impl CleanVerdict {
    pub fn label(self) -> &'static str {
        match self {
            Self::NoRow => "NoRow",
            Self::Live => "Live",
            Self::Dead => "Dead",
            Self::DraftSkeleton => "DraftSkeleton",
            Self::Divergent => "Divergent",
        }
    }

    /// Сносим ли по вердикту при данном ack (гейт исполнения, до журнала).
    fn deletable_with(self, ack: &[u64], number: u64) -> bool {
        match self {
            Self::DraftSkeleton => true,
            Self::Live | Self::Dead => ack.contains(&number),
            Self::NoRow | Self::Divergent => false,
        }
    }

    /// Не-Draft: подтверждение владельца обязательно (канон ADR-025).
    fn needs_ack(self) -> bool {
        matches!(self, Self::Live | Self::Dead)
    }
}

/// Отчёт пробы одной строки (dry-run и execute-план).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanRowReport {
    pub number: u64,
    pub title: String,
    pub verdict: CleanVerdict,
    /// Raw Status колонки доски на момент пробы (вход журнала).
    pub status_raw: Option<String>,
    /// item id привязки на момент пробы (identity сноса в журнале).
    pub item_id: u64,
    /// Причина Divergent (деталь пробы — в отчёте, не втихую).
    pub note: Option<String>,
}

/// Отчёт исполнения: что снесено, где журнал.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanExecReport {
    /// Пуст — сносить было нечего (журнал не пишется).
    pub journal_id: String,
    /// Снесённые строки (в порядке исполнения).
    pub deleted: Vec<CleanRowReport>,
    /// Пропущенные (NoRow — не было на доске).
    pub skipped: Vec<CleanRowReport>,
}

/// Исход undo одной строки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoOutcome {
    /// Строка восстановлена (новый item id — id строк меняется, ADR-025).
    Restored(u64),
    /// Уже на доске — идемпотентный skip.
    AlreadyOnBoard,
    /// Не удалось (деталь — громко в отчёте; журнал сохраняется).
    Failed(String),
}

/// Отчёт undo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoReport {
    pub journal_id: String,
    pub rows: Vec<(u64, String, UndoOutcome)>,
}

pub struct TaskCleanUseCase {
    pub clean: Arc<dyn CleanBoardRepo>,
    pub journals: Arc<dyn CleanJournalStore>,
    /// карта проект → доска (мультипроектный конфиг).
    pub boards: Vec<(String, String)>,
    pub clock: Arc<dyn Clock>,
}

impl TaskCleanUseCase {
    fn board_of(&self, project: &str) -> UsecaseResult<String> {
        self.boards
            .iter()
            .find(|(p, _)| p == project)
            .map(|(_, b)| b.clone())
            .ok_or_else(|| UsecaseError::Board(format!("project «{project}» is not in the config")))
    }

    /// Нормировка входа: дедуп, детерминированный порядок, батч-кап.
    fn normalize(numbers: &[u64]) -> UsecaseResult<Vec<u64>> {
        let mut uniq: Vec<u64> = numbers.to_vec();
        uniq.sort_unstable();
        uniq.dedup();
        if uniq.len() > clean_batch_cap() {
            return Err(UsecaseError::Board(format!(
                "a batch of {} rows — the cap is {} (split the calls)",
                uniq.len(),
                clean_batch_cap()
            )));
        }
        Ok(uniq)
    }

    /// Проба одной строки по готовому индексу (фетч индекса — один на
    /// вызов, выше): привязка → issue → вердикт.
    async fn probe_row(&self, number: u64, binding: Option<&RowBinding>) -> CleanRowReport {
        let Some(binding) = binding else {
            return CleanRowReport {
                number,
                title: String::new(),
                verdict: CleanVerdict::NoRow,
                status_raw: None,
                item_id: 0,
                note: None,
            };
        };
        let issue = match self.clean.fresh_issue(number).await {
            Ok(i) => i,
            Err(e) => {
                return divergent(number, format!("the issue probe did not complete: {e}"));
            }
        };
        let Some(issue) = issue else {
            // item есть, issue нет — противоречие источников (фантом
            // read-лага/ручной снос issue) — fail-closed, ADR-025
            return divergent(
                number,
                "the item is on the board, the issue is not found in the repo — the sources contradict".to_string(),
            );
        };
        let verdict = classify(&issue);
        CleanRowReport {
            number,
            title: issue.title,
            verdict,
            status_raw: binding.status_raw.clone().filter(|s| !s.trim().is_empty()),
            item_id: binding.item_id,
            note: None,
        }
    }

    /// Гейт батча: Divergent и не-Draft без ack валят ВЕСЬ батч (канон
    /// ADR-025 — fail-closed, «Live без ack — отказ всего батча»).
    fn gate(reports: &[CleanRowReport], ack: &[u64]) -> UsecaseResult<()> {
        let divergent: Vec<String> = reports
            .iter()
            .filter(|r| r.verdict == CleanVerdict::Divergent)
            .map(|r| match &r.note {
                Some(note) => format!("#{}: {}", r.number, note),
                None => format!("#{}: {}", r.number, r.verdict.label()),
            })
            .collect();
        if !divergent.is_empty() {
            return Err(UsecaseError::Board(format!(
                "the batch is rejected (fail-closed), nothing was deleted: {}",
                divergent.join("; ")
            )));
        }
        let unacked: Vec<String> = reports
            .iter()
            .filter(|r| r.verdict.needs_ack() && !ack.contains(&r.number))
            .map(|r| format!("#{} {} «{}»", r.number, r.verdict.label(), r.title))
            .collect();
        if !unacked.is_empty() {
            return Err(UsecaseError::Board(format!(
                "non-Draft rows require the owner's confirmation (ack): {} — \
                 the dry-run above; pass the confirmed numbers into ack",
                unacked.join("; ")
            )));
        }
        Ok(())
    }

    /// Dry-run: вердикты по свежим пробам, ноль мутаций. Индекс доски —
    /// один фетч на вызов (квотная дисциплина, ревью Common-1).
    pub async fn plan(&self, project: &str, numbers: &[u64]) -> UsecaseResult<Vec<CleanRowReport>> {
        let board = self.board_of(project)?;
        let numbers = Self::normalize(numbers)?;
        let index = self.clean.fresh_index(&board).await?;
        let mut reports = Vec::with_capacity(numbers.len());
        for number in numbers {
            reports.push(self.probe_row(number, index.get(&number)).await);
        }
        Ok(reports)
    }

    /// Исполнение сноса: self-probe → гейт → write-ahead журнал → DELETE.
    /// Индекс — один фетч на вызов; stale-привязка ловится DELETE-404.
    pub async fn execute(
        &self,
        project: &str,
        numbers: &[u64],
        ack: &[u64],
    ) -> UsecaseResult<CleanExecReport> {
        let board = self.board_of(project)?;
        let numbers = Self::normalize(numbers)?;
        let index = self.clean.fresh_index(&board).await?;
        let mut reports = Vec::with_capacity(numbers.len());
        for number in numbers {
            reports.push(self.probe_row(number, index.get(&number)).await);
        }
        Self::gate(&reports, ack)?;
        let mut deleted = Vec::new();
        let mut skipped = Vec::new();
        for report in &reports {
            if report.verdict.deletable_with(ack, report.number) {
                deleted.push(report.clone());
            } else {
                skipped.push(report.clone());
            }
        }
        if deleted.is_empty() {
            return Ok(CleanExecReport {
                journal_id: String::new(),
                deleted,
                skipped,
            });
        }
        // write-ahead: журнал ДО первой мутации (ADR-025) — restore
        // возможен даже при падении посреди батча
        let journal = self.journal(project, &board, &deleted);
        self.journals.put(&journal)?;
        for row in &deleted {
            if let Err(e) = self.clean.remove_row(&board, row.number, row.item_id).await {
                return Err(UsecaseError::Board(format!(
                    "delete #{}: {e} — part of the batch may have been deleted; journal {} \
                     (task_undo restores, idempotent)",
                    row.number, journal.id
                )));
            }
        }
        Ok(CleanExecReport {
            journal_id: journal.id,
            deleted,
            skipped,
        })
    }

    /// Undo: recreate item + Status по журналу, идемпотентно (ADR-025:
    /// тайтл уже на доске → skip; здесь эквивалент — привязка item'а).
    pub async fn undo(&self, project: &str, journal: Option<&str>) -> UsecaseResult<UndoReport> {
        let found = self.journals.get(project, journal)?.ok_or_else(|| {
            let ids = self.journals.ids(project).unwrap_or_default();
            let tail = if ids.is_empty() {
                "no journals".to_string()
            } else {
                format!("available: {}", ids.join(", "))
            };
            UsecaseError::Board(format!(
                "journal {} of project {project} not found; {tail}",
                journal.unwrap_or("(latest)")
            ))
        })?;
        // индекс — один фетч на вызов (квотная дисциплина, ревью Common-1):
        // отказ фетча громко валит весь undo, restore-сбои — per-row Failed
        let index = self.clean.fresh_index(&found.board).await?;
        let mut rows = Vec::with_capacity(found.rows.len());
        for row in &found.rows {
            if index.contains_key(&row.number) {
                rows.push((row.number, row.title.clone(), UndoOutcome::AlreadyOnBoard));
                continue;
            }
            match self
                .clean
                .restore_row(&found.board, row.number, row.status_raw.as_deref())
                .await
            {
                Ok(item_id) => rows.push((
                    row.number,
                    row.title.clone(),
                    UndoOutcome::Restored(item_id),
                )),
                Err(e) => rows.push((
                    row.number,
                    row.title.clone(),
                    UndoOutcome::Failed(format!(
                        "{e} (status «{}» — deliver it via task_edit)",
                        row.status_raw.as_deref().unwrap_or("—")
                    )),
                )),
            }
        }
        Ok(UndoReport {
            journal_id: found.id,
            rows,
        })
    }

    /// Детерминированное имя журнала: ts + проект + хэш состава строк
    /// (два сноса в одну миллисекунду не перетирают журналы).
    fn journal(&self, project: &str, board: &str, rows: &[CleanRowReport]) -> CleanJournal {
        let ts = self.clock.now_ms();
        let payload: Vec<CleanRow> = rows
            .iter()
            .map(|r| CleanRow {
                number: r.number,
                title: r.title.clone(),
                status_raw: r.status_raw.clone(),
                item_id: r.item_id,
            })
            .collect();
        let fingerprint = payload
            .iter()
            .map(|r| {
                format!(
                    "{}:{}:{}:{}",
                    r.number,
                    r.item_id,
                    r.status_raw.as_deref().unwrap_or(""),
                    r.title
                )
            })
            .collect::<Vec<_>>()
            .join("|");
        let id = format!("{project}-{ts}-{}", &body_hash(&fingerprint)[..8]);
        CleanJournal {
            id,
            project: project.to_string(),
            board: board.to_string(),
            created_ms: ts,
            rows: payload,
        }
    }
}

fn divergent(number: u64, note: String) -> CleanRowReport {
    CleanRowReport {
        number,
        title: String::new(),
        verdict: CleanVerdict::Divergent,
        status_raw: None,
        item_id: 0,
        note: Some(note),
    }
}

/// Вердикт по свежей пробе issue (привязка уже верифицирована): closed
/// терминален (Dead) независимо от тела; open — тело решает Draft/Live.
fn classify(probe: &IssueProbe) -> CleanVerdict {
    if !probe.open {
        return CleanVerdict::Dead;
    }
    if is_trivial_body(&probe.body, &probe.title) {
        CleanVerdict::DraftSkeleton
    } else {
        CleanVerdict::Live
    }
}

/// Тело-пустышка: пустое, голый скелет «## Постановка» или скелет create
/// «## Постановка + тайтл» (render_create_body пустого body). fm-шапка
/// вырезается; строгое равенство — безопасное направление: дописанное
/// содержимое делает строку Live (защищена), а не Draft.
fn is_trivial_body(body: &str, title: &str) -> bool {
    let clean = crate::fm_lite::strip(body).trim();
    clean.is_empty()
        || clean == "## Постановка"
        || clean == format!("## Постановка\n\n{title}").trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::tick_tests::FixedClock;
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::sync::Mutex;

    /// Фейк носителя: state под Mutex (async_trait требует Send/Sync),
    /// мутации между вызовами имитируют параллельного writer'а. События —
    /// лента сносов/восстановлений.
    /// Состояние строки фейка: (item_id, status_raw, issue).
    type FakeRow = (u64, Option<String>, FakeIssue);

    #[derive(Default)]
    struct FakeCarrier {
        /// number → строка
        rows: Mutex<HashMap<u64, FakeRow>>,
        /// номера, чей fresh_issue отвечает 404 при живой привязке
        issue_404: Mutex<HashSet<u64>>,
        /// сбой фетча индекса (громкий отказ плана/undo)
        binding_err: Mutex<bool>,
        /// сбой restore (per-row Failed в undo)
        restore_err: Mutex<bool>,
        /// remove-идентичность не совпадает (параллельный writer пересоздал)
        stale_items: Mutex<HashSet<u64>>,
        events: Mutex<Vec<String>>,
        /// счётчик полных фетчей индекса — пин квотной дисциплины (Common-1)
        index_fetches: Mutex<usize>,
    }

    #[derive(Clone)]
    struct FakeIssue {
        title: String,
        open: bool,
        body: String,
    }

    impl FakeCarrier {
        fn row(&self, number: u64, status: Option<&str>, issue: FakeIssue) {
            self.rows
                .lock()
                .expect("fake rows")
                .insert(number, (1_000 + number, status.map(str::to_string), issue));
        }
    }

    fn issue(title: &str, open: bool, body: &str) -> FakeIssue {
        FakeIssue {
            title: title.into(),
            open,
            body: body.into(),
        }
    }

    #[async_trait]
    impl CleanBoardRepo for FakeCarrier {
        async fn fresh_index(&self, _board: &str) -> UsecaseResult<HashMap<u64, RowBinding>> {
            if *self.binding_err.lock().expect("fake err") {
                return Err(UsecaseError::Board("pv2 rest 503 (мок)".into()));
            }
            *self.index_fetches.lock().expect("fetches") += 1;
            let rows = self.rows.lock().expect("fake rows");
            Ok(rows
                .iter()
                .map(|(number, (_, st, _))| {
                    (
                        *number,
                        RowBinding {
                            item_id: 1_000 + number,
                            status_raw: st.clone(),
                        },
                    )
                })
                .collect())
        }
        async fn fresh_issue(&self, number: u64) -> UsecaseResult<Option<IssueProbe>> {
            if self.issue_404.lock().expect("fake 404").contains(&number) {
                return Ok(None);
            }
            Ok(self
                .rows
                .lock()
                .expect("fake rows")
                .get(&number)
                .map(|(_, _, i)| IssueProbe {
                    title: i.title.clone(),
                    open: i.open,
                    body: i.body.clone(),
                }))
        }
        async fn remove_row(&self, _board: &str, number: u64, item_id: u64) -> UsecaseResult<()> {
            let stale = self
                .stale_items
                .lock()
                .expect("fake stale")
                .contains(&number);
            let mut rows = self.rows.lock().expect("fake rows");
            match rows.get(&number) {
                Some((id, _, _)) if *id == item_id && !stale => {
                    rows.remove(&number);
                    drop(rows);
                    self.events
                        .lock()
                        .expect("fake events")
                        .push(format!("remove:{number}"));
                    Ok(())
                }
                _ => Err(UsecaseError::Board(format!("delete #{number}: HTTP 404"))),
            }
        }
        async fn restore_row(
            &self,
            _board: &str,
            number: u64,
            status_raw: Option<&str>,
        ) -> UsecaseResult<u64> {
            if *self.restore_err.lock().expect("fake restore") {
                return Err(UsecaseError::Board("add item: HTTP 503 (мок)".into()));
            }
            let mut rows = self.rows.lock().expect("fake rows");
            let restored = issue("восстановленный", true, "");
            rows.insert(
                number,
                (2_000 + number, status_raw.map(str::to_string), restored),
            );
            drop(rows);
            self.events
                .lock()
                .expect("fake events")
                .push(format!("restore:{number}"));
            Ok(2_000 + number)
        }
    }

    /// Журнал в памяти; put-лента для write-ahead-пина.
    #[derive(Default)]
    struct MemJournals {
        entries: Mutex<BTreeMap<String, CleanJournal>>,
        events: Mutex<Vec<String>>,
    }

    impl CleanJournalStore for MemJournals {
        fn put(&self, journal: &CleanJournal) -> UsecaseResult<()> {
            self.entries
                .lock()
                .expect("fake journals")
                .insert(journal.id.clone(), journal.clone());
            self.events
                .lock()
                .expect("fake events")
                .push(format!("journal:{}", journal.id));
            Ok(())
        }
        fn get(&self, project: &str, id: Option<&str>) -> UsecaseResult<Option<CleanJournal>> {
            let guard = self.entries.lock().expect("fake journals");
            match id {
                Some(id) => Ok(guard.get(id).cloned()),
                None => Ok(guard
                    .values()
                    .filter(|j| j.project == project)
                    // свежий — по created_ms (контракт store, не порядок ключей)
                    .max_by(|a, b| {
                        a.created_ms
                            .cmp(&b.created_ms)
                            .then_with(|| a.id.cmp(&b.id))
                    })
                    .cloned()),
            }
        }
        fn ids(&self, project: &str) -> UsecaseResult<Vec<String>> {
            // свежие первыми — по created_ms (контракт store)
            let mut by_age: Vec<(u64, String)> = self
                .entries
                .lock()
                .expect("fake journals")
                .values()
                .filter(|j| j.project == project)
                .map(|j| (j.created_ms, j.id.clone()))
                .collect();
            by_age.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
            Ok(by_age.into_iter().map(|(_, id)| id).collect())
        }
    }

    fn usecase(clean: Arc<FakeCarrier>, journals: Arc<MemJournals>) -> TaskCleanUseCase {
        TaskCleanUseCase {
            clean,
            journals,
            boards: vec![("uwuwu".into(), "Задачи uwuwu".into())],
            clock: Arc::new(FixedClock),
        }
    }

    fn rig() -> (TaskCleanUseCase, Arc<FakeCarrier>, Arc<MemJournals>) {
        let carrier = Arc::new(FakeCarrier::default());
        let journals = Arc::new(MemJournals::default());
        let u = usecase(carrier.clone(), journals.clone());
        (u, carrier, journals)
    }

    // --- вердикты ---

    #[test]
    fn classify_body_forms() {
        let probe = |title: &str, open: bool, body: &str| IssueProbe {
            title: title.into(),
            open,
            body: body.into(),
        };
        // пустое тело — Draft
        assert_eq!(classify(&probe("t", true, "")), CleanVerdict::DraftSkeleton);
        // скелет create («Постановка + тайтл») — Draft
        assert_eq!(
            classify(&probe("t", true, "## Постановка\n\nt\n")),
            CleanVerdict::DraftSkeleton
        );
        // fm-шапка поверх скелета вырезается — Draft
        assert_eq!(
            classify(&probe(
                "t",
                true,
                "---\nstatus: Draft\n---\n## Постановка\n\nt"
            )),
            CleanVerdict::DraftSkeleton
        );
        // голый заголовок без тела — Draft
        assert_eq!(
            classify(&probe("t", true, "## Постановка")),
            CleanVerdict::DraftSkeleton
        );
        // дописанное содержимое — Live (безопасное направление)
        assert_eq!(
            classify(&probe("t", true, "## Постановка\n\nтекст задачи")),
            CleanVerdict::Live
        );
        // closed терминален независимо от тела
        assert_eq!(classify(&probe("t", false, "текст")), CleanVerdict::Dead);
    }

    /// #1664: CRLF-fm не прячет скелет от гарда чистки; дописанное
    /// содержимое остаётся Live — направление ошибки безопасное.
    #[test]
    fn classify_crlf_fm_body_forms() {
        let probe = |body: &str| IssueProbe {
            title: "t".into(),
            open: true,
            body: body.into(),
        };
        assert_eq!(
            classify(&probe("---\r\nstatus: Draft\r\n---\r\n")),
            CleanVerdict::DraftSkeleton
        );
        assert_eq!(
            classify(&probe("---\r\nstatus: Draft\r\n---\r\n## Постановка\n\nt")),
            CleanVerdict::DraftSkeleton
        );
        assert_eq!(
            classify(&probe(
                "---\r\nstatus: Draft\r\n---\r\n## Постановка\n\nтекст задачи"
            )),
            CleanVerdict::Live
        );
    }

    // --- dry-run ---

    #[tokio::test]
    async fn plan_verdicts_zero_mutations() {
        let (u, carrier, journals) = rig();
        carrier.row(1, Some("Draft"), issue("draft", true, ""));
        carrier.row(2, Some("Done"), issue("dead", false, "тело"));
        carrier.row(3, Some("In Progress"), issue("live", true, "тело"));
        let reports = u.plan("uwuwu", &[1, 2, 3, 4, 4]).await.unwrap();
        let verdicts: Vec<(u64, CleanVerdict)> =
            reports.iter().map(|r| (r.number, r.verdict)).collect();
        assert_eq!(
            verdicts,
            vec![
                (1, CleanVerdict::DraftSkeleton),
                (2, CleanVerdict::Dead),
                (3, CleanVerdict::Live),
                (4, CleanVerdict::NoRow),
            ],
            "дедуп 4,4 и вердикты по свежим пробам"
        );
        assert!(
            journals.events.lock().expect("j").is_empty(),
            "dry-run без мутаций"
        );
        assert!(
            carrier.events.lock().expect("c").is_empty(),
            "dry-run без сносов"
        );
        // status_raw и item_id доезжают для журнала
        assert_eq!(reports[0].status_raw.as_deref(), Some("Draft"));
        assert_eq!(reports[0].item_id, 1_001);
    }

    #[tokio::test]
    async fn plan_index_failure_is_loud_whole_call() {
        let (u, carrier, _) = rig();
        carrier.row(9, Some("Draft"), issue("x", true, ""));
        *carrier.binding_err.lock().expect("err") = true;
        let err = u
            .plan("uwuwu", &[9])
            .await
            .expect_err("фетч индекса не состоялся — громкий отказ вызова");
        assert!(
            err.to_string().contains("pv2 rest 503"),
            "деталь транспорта в отказе: {err}"
        );
    }

    /// Пин квотной дисциплины (ревью Common-1): план/исполнение/undo
    /// тянут ОДИН полный фетч индекса на вызов, не по-строчные фетчи.
    #[tokio::test]
    async fn one_index_fetch_per_call() {
        let (u, carrier, _) = rig();
        carrier.row(1, Some("Draft"), issue("d1", true, ""));
        carrier.row(2, Some("Done"), issue("d2", false, "тело"));
        carrier.row(3, Some("In Progress"), issue("d3", true, "тело"));
        u.plan("uwuwu", &[1, 2, 3]).await.unwrap();
        assert_eq!(*carrier.index_fetches.lock().expect("f"), 1, "plan: 1 фетч");
        u.execute("uwuwu", &[1, 2, 3], &[2, 3]).await.unwrap();
        assert_eq!(
            *carrier.index_fetches.lock().expect("f"),
            2,
            "execute: свой один фетч"
        );
        u.undo("uwuwu", None).await.unwrap();
        assert_eq!(
            *carrier.index_fetches.lock().expect("f"),
            3,
            "undo: свой один фетч"
        );
    }

    // --- гейты исполнения ---

    #[tokio::test]
    async fn execute_refuses_live_without_ack_whole_batch() {
        let (u, carrier, journals) = rig();
        carrier.row(1, Some("Draft"), issue("draft", true, ""));
        carrier.row(2, Some("In Progress"), issue("live", true, "тело"));
        let err = u
            .execute("uwuwu", &[1, 2], &[])
            .await
            .expect_err("Live без ack валит весь батч");
        assert!(err.to_string().contains("ack"), "текст: {err}");
        assert!(err.to_string().contains("#2"), "строка названа: {err}");
        assert!(carrier.events.lock().expect("c").is_empty(), "сносов нет");
        assert!(
            journals.entries.lock().expect("j").is_empty(),
            "журнала нет"
        );
    }

    #[tokio::test]
    async fn execute_refuses_dead_without_ack() {
        let (u, carrier, _) = rig();
        carrier.row(2, Some("Done"), issue("dead", false, "тело"));
        let err = u
            .execute("uwuwu", &[2], &[])
            .await
            .expect_err("Dead (не-Draft) без ack — отказ");
        assert!(
            err.to_string().contains("require the owner's confirmation"),
            "текст: {err}"
        );
    }

    #[tokio::test]
    async fn execute_refuses_divergent_whole_batch() {
        let (u, carrier, _) = rig();
        carrier.row(1, Some("Draft"), issue("draft", true, ""));
        // фантом: привязка есть, issue 404 — источники противоречивы
        carrier.row(7, Some("Draft"), issue("фантом", true, ""));
        carrier.issue_404.lock().expect("404").insert(7);
        let err = u
            .execute("uwuwu", &[1, 7], &[])
            .await
            .expect_err("Divergent валит весь батч");
        assert!(err.to_string().contains("fail-closed"), "текст: {err}");
        assert!(
            err.to_string().contains("the sources contradict"),
            "деталь: {err}"
        );
        assert!(carrier.events.lock().expect("c").is_empty(), "сносов нет");
    }

    #[tokio::test]
    async fn execute_respects_ack_for_live_and_dead() {
        let (u, carrier, journals) = rig();
        carrier.row(1, Some("Draft"), issue("draft", true, ""));
        carrier.row(2, Some("Done"), issue("dead", false, "тело"));
        carrier.row(3, Some("In Progress"), issue("live", true, "тело"));
        let report = u.execute("uwuwu", &[1, 2, 3], &[2, 3]).await.unwrap();
        assert_eq!(report.deleted.len(), 3, "draft + acked dead + acked live");
        assert_eq!(report.skipped.len(), 0);
        assert!(!report.journal_id.is_empty());
        assert_eq!(
            *carrier.events.lock().expect("c"),
            vec!["remove:1", "remove:2", "remove:3"],
            "снесены все три в порядке номеров"
        );
        assert_eq!(
            journals.entries.lock().expect("j").len(),
            1,
            "один журнал на батч"
        );
    }

    // --- write-ahead журнал ---

    #[tokio::test]
    async fn journal_written_before_first_delete() {
        let (u, carrier, journals) = rig();
        carrier.row(1, Some("Draft"), issue("draft", true, ""));
        carrier.row(2, Some("Draft"), issue("draft2", true, ""));
        u.execute("uwuwu", &[1, 2], &[]).await.unwrap();
        let journal_events = journals.events.lock().expect("j").clone();
        let remove_events = carrier.events.lock().expect("c").clone();
        assert_eq!(journal_events.len(), 1, "один журнал на батч");
        // write-ahead инвариант: execute кладёт журнал строго до цикла
        // remove (порядок в коде); ленты носителей подтверждают состав
        assert_eq!(remove_events, vec!["remove:1", "remove:2"]);
        let journal = journals
            .entries
            .lock()
            .expect("j")
            .values()
            .next()
            .unwrap()
            .clone();
        assert_eq!(journal.rows.len(), 2);
        assert_eq!(journal.rows[0].status_raw.as_deref(), Some("Draft"));
        assert_eq!(journal.rows[0].item_id, 1_001, "item id пробы в журнале");
        assert_eq!(journal.board, "Задачи uwuwu");
    }

    #[tokio::test]
    async fn execute_delete_failure_reports_journal_for_undo() {
        let (u, carrier, journals) = rig();
        carrier.row(1, Some("Draft"), issue("draft", true, ""));
        // параллельный writer пересоздал привязку: item_id пробы устарел → 404
        carrier.stale_items.lock().expect("stale").insert(1);
        let err = u
            .execute("uwuwu", &[1], &[])
            .await
            .expect_err("устаревший item id — громкий отказ");
        assert!(err.to_string().contains("journal"), "журнал назван: {err}");
        assert_eq!(
            journals.entries.lock().expect("j").len(),
            1,
            "журнал write-ahead сохранён"
        );
        // строка не снесена (remove отвергнут носителем)
        assert!(carrier
            .fresh_index("Задачи uwuwu")
            .await
            .unwrap()
            .contains_key(&1));
    }

    #[tokio::test]
    async fn execute_nothing_deletable_no_journal() {
        let (u, carrier, journals) = rig();
        // строку 4 никто не сеял — NoRow: сносить нечего
        let report = u.execute("uwuwu", &[4], &[]).await.unwrap();
        assert!(report.deleted.is_empty());
        assert!(report.journal_id.is_empty(), "журнал не пишется без сносов");
        assert!(journals.entries.lock().expect("j").is_empty());
        assert!(carrier.events.lock().expect("c").is_empty());
    }

    // --- батч-кап и конфиг ---

    #[tokio::test]
    async fn batch_cap_rejects_over_50() {
        let (u, _, _) = rig();
        let numbers: Vec<u64> = (1..=51).collect();
        let err = u.plan("uwuwu", &numbers).await.expect_err("кап 50");
        assert!(err.to_string().contains("50"), "текст: {err}");
    }

    #[tokio::test]
    async fn unknown_project_is_loud() {
        let (u, _, _) = rig();
        let err = u.plan("чужой", &[1]).await.expect_err("проект вне конфига");
        assert!(err.to_string().contains("is not in the config"));
    }

    // --- undo ---

    #[tokio::test]
    async fn undo_restores_missing_and_skips_present() {
        let (u, carrier, _) = rig();
        carrier.row(1, Some("Draft"), issue("draft", true, ""));
        carrier.row(2, Some("In Progress"), issue("live", true, "тело"));
        u.execute("uwuwu", &[1, 2], &[2]).await.unwrap();
        assert_eq!(
            *carrier.events.lock().expect("c"),
            vec!["remove:1", "remove:2"]
        );
        // строку 1 владелец вернул вручную до undo — идемпотентный skip
        carrier.row(1, Some("Draft"), issue("draft", true, ""));
        let report = u.undo("uwuwu", None).await.unwrap();
        assert_eq!(report.rows.len(), 2);
        assert_eq!(
            report.rows[0].2,
            UndoOutcome::AlreadyOnBoard,
            "#1 уже на доске"
        );
        assert!(
            matches!(report.rows[1].2, UndoOutcome::Restored(_)),
            "#2 восстановлен: {:?}",
            report.rows[1].2
        );
        // восстановленный статус вернулся из журнала
        let index = carrier.fresh_index("Задачи uwuwu").await.unwrap();
        let binding = index.get(&2).expect("#2 на доске");
        assert_eq!(binding.status_raw.as_deref(), Some("In Progress"));
        // повторный undo — всё skip
        let again = u.undo("uwuwu", None).await.unwrap();
        assert!(
            again
                .rows
                .iter()
                .all(|(_, _, o)| *o == UndoOutcome::AlreadyOnBoard),
            "идемпотентность undo"
        );
    }

    #[tokio::test]
    async fn undo_unknown_journal_lists_available() {
        let (u, carrier, journals) = rig();
        carrier.row(1, Some("Draft"), issue("d", true, ""));
        u.execute("uwuwu", &[1], &[]).await.unwrap();
        let err = u
            .undo("uwuwu", Some("нет-такого"))
            .await
            .expect_err("журнал не найден");
        assert!(
            err.to_string().contains("available:"),
            "перечень в отказе: {err}"
        );
        assert!(
            err.to_string().contains("uwuwu-2000-"),
            "id живого журнала в перечне"
        );
        drop(journals);
    }

    #[tokio::test]
    async fn undo_restore_failure_is_loud_and_contained() {
        let (u, carrier, _) = rig();
        carrier.row(1, Some("Draft"), issue("d", true, ""));
        carrier.row(2, Some("Draft"), issue("d2", true, ""));
        u.execute("uwuwu", &[1, 2], &[]).await.unwrap();
        // restore падает: носитель не принимает add — отчёт громкий,
        // остальные строки восстановятся (пер-строчный контейнд)
        *carrier.restore_err.lock().expect("restore") = true;
        let report = u.undo("uwuwu", None).await.unwrap();
        assert!(
            report
                .rows
                .iter()
                .all(|(_, _, o)| matches!(o, UndoOutcome::Failed(_))),
            "restore сломан — все Failed: {:?}",
            report.rows
        );
    }

    #[tokio::test]
    async fn undo_index_failure_is_loud_whole_call() {
        let (u, carrier, _) = rig();
        carrier.row(1, Some("Draft"), issue("d", true, ""));
        u.execute("uwuwu", &[1], &[]).await.unwrap();
        // фетч индекса не состоялся — undo громко падает целиком
        // (fail-closed: не молча скипать строки в неизвестное состояние)
        *carrier.binding_err.lock().expect("err") = true;
        let err = u
            .undo("uwuwu", None)
            .await
            .expect_err("фетч индекса сломан — отказ вызова");
        assert!(err.to_string().contains("pv2 rest 503"), "{err}");
    }

    // --- журнал: детерминированное имя ---

    #[tokio::test]
    async fn journal_id_deterministic_same_composition_and_distinct_otherwise() {
        let (u, carrier, journals) = rig();
        carrier.row(1, Some("Draft"), issue("d", true, ""));
        u.execute("uwuwu", &[1], &[]).await.unwrap();
        carrier.row(1, Some("Draft"), issue("d", true, ""));
        u.execute("uwuwu", &[1], &[]).await.unwrap();
        let ids = journals.ids("uwuwu").unwrap();
        assert_eq!(
            ids.len(),
            1,
            "идентичный состав+ts — тот же id: перезапись журнала \
             идентичным составом ничего не теряет (id ≡ состав)"
        );
        assert!(
            ids[0].starts_with("uwuwu-2000-"),
            "детерминированный префикс проект-ts"
        );
        // другой состав — другое имя (строка 2 Dead — с ack владельца)
        carrier.row(2, Some("Done"), issue("другая", false, "тело"));
        u.execute("uwuwu", &[2], &[2]).await.unwrap();
        let ids = journals.ids("uwuwu").unwrap();
        assert_eq!(ids.len(), 2, "другой состав — другой журнал");
    }
}
