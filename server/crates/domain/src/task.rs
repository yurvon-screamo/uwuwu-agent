//! Агрегат Task: строка доски + сессии стадий + переходы статусов.
//! Чистый домен: ноль зависимостей. Инварианты канона — здесь и в тестах.

/// Статусы канона досок uwuwu. Empty — строка без статуса
/// (легаси-черновик владельца: конвейер не трогает).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Draft,
    Todo,
    Design,
    ReviewPlan,
    InProgress,
    ReviewResult,
    Testing,
    UserReview,
    Done,
    NeedsAttention,
    Wait,
    Deleted,
    Empty,
}

impl Status {
    pub fn parse(raw: &str) -> Self {
        match raw.trim() {
            "Draft" => Self::Draft,
            "Todo" => Self::Todo,
            "Design" => Self::Design,
            "Ревью плана" => Self::ReviewPlan,
            "In Progress" => Self::InProgress,
            "Ревью результата" => Self::ReviewResult,
            "Тестирование" => Self::Testing,
            "Review" => Self::UserReview,
            "Done" => Self::Done,
            "Needs attention" => Self::NeedsAttention,
            "Вейт" => Self::Wait,
            "Deleted" => Self::Deleted,
            "" => Self::Empty,
            // неизвестное имя — пустой статус: строка выпадает из рабочих
            // списков (легаси/мусор); логирование сырых имён — дело инфры
            _ => Self::Empty,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "Draft",
            Self::Todo => "Todo",
            Self::Design => "Design",
            Self::ReviewPlan => "Ревью плана",
            Self::InProgress => "In Progress",
            Self::ReviewResult => "Ревью результата",
            Self::Testing => "Тестирование",
            Self::UserReview => "Review",
            Self::Done => "Done",
            Self::NeedsAttention => "Needs attention",
            Self::Wait => "Вейт",
            Self::Deleted => "Deleted",
            Self::Empty => "",
        }
    }

    /// LLM-тратящий статус: занимает слот CUP.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Design | Self::ReviewPlan | Self::InProgress | Self::ReviewResult | Self::Testing
        )
    }

    /// Терминальные: конвейер больше не трогает (Deleted — мягкое удаление).
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Deleted)
    }

    /// Канон переходов (AGENTS.md «статусы кто ставит» + пайп конвейера):
    /// рёбра пайплайна + Needs attention из любой рабочей стадии; Done —
    /// только через close/автодон (не task_edit); Done/Deleted/Empty —
    /// без исходящих рёбер (реопен/легаси — владелец в UI, мимо MCP).
    pub fn can_transition(from: Self, to: Self) -> bool {
        use Status as S;
        if from == to {
            return true; // идемпотентная переустановка
        }
        matches!(
            (from, to),
            (S::Draft, S::Todo)
                | (S::Todo, S::Design | S::InProgress)
                | (S::Wait, S::Todo | S::Design | S::InProgress)
                | (S::Design, S::ReviewPlan | S::InProgress | S::NeedsAttention)
                | (S::ReviewPlan, S::InProgress | S::NeedsAttention)
                | (
                    S::InProgress,
                    S::ReviewPlan | S::ReviewResult | S::NeedsAttention
                )
                | (
                    S::ReviewResult,
                    S::InProgress | S::Testing | S::NeedsAttention
                )
                | (
                    S::Testing,
                    S::UserReview | S::InProgress | S::NeedsAttention
                )
                | (S::UserReview, S::InProgress | S::NeedsAttention)
                | (S::NeedsAttention, S::InProgress)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Priority {
    High,
    Medium,
    Low,
    /// Не задан — ниже low в очереди CUP.
    #[default]
    Empty,
}

impl Priority {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "high" => Self::High,
            "medium" => Self::Medium,
            "low" => Self::Low,
            _ => Self::Empty,
        }
    }

    /// Порядок очереди: меньше — раньше.
    pub fn rank(self) -> u8 {
        match self {
            Self::High => 0,
            Self::Medium => 1,
            Self::Low => 2,
            Self::Empty => 3,
        }
    }
}

/// Слот сессии стадии (ячейки «Сессия dev/ревью/тест»).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Slot {
    Design,
    Dev,
    PlanReview,
    ResultReview,
    Test,
}

/// Нативные поля issue, типизированные и инкапсулированные:
/// домен владеет мутациями, инфра маппит имена → конструкторы.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskFields {
    dev_session: Option<SessionRef>,
    review_session: Option<SessionRef>,
    test_session: Option<SessionRef>,
    branch: Option<BranchName>,
    pr: Option<PrUrl>,
}

impl TaskFields {
    pub fn new(
        dev_session: Option<SessionRef>,
        review_session: Option<SessionRef>,
        test_session: Option<SessionRef>,
        branch: Option<BranchName>,
        pr: Option<PrUrl>,
    ) -> Self {
        Self {
            dev_session,
            review_session,
            test_session,
            branch,
            pr,
        }
    }

    pub fn session(&self, slot: Slot) -> Option<&SessionRef> {
        match slot {
            Slot::Dev | Slot::Design => self.dev_session.as_ref(),
            Slot::PlanReview | Slot::ResultReview => self.review_session.as_ref(),
            Slot::Test => self.test_session.as_ref(),
        }
    }

    pub fn branch(&self) -> Option<&BranchName> {
        self.branch.as_ref()
    }

    pub fn pr(&self) -> Option<&PrUrl> {
        self.pr.as_ref()
    }

    /// Мутатор: заявка сессии в слот (инфра-маппинг полей и спавны).
    pub fn set_session(&mut self, slot: Slot, session: SessionRef) {
        match slot {
            Slot::Dev | Slot::Design => self.dev_session = Some(session),
            Slot::PlanReview | Slot::ResultReview => self.review_session = Some(session),
            Slot::Test => self.test_session = Some(session),
        }
    }

    /// Мутатор: заявка PR-ячейки.
    pub fn set_pr(&mut self, url: PrUrl) {
        self.pr = Some(url);
    }

    /// Мутатор: заявка ветки.
    pub fn set_branch(&mut self, name: BranchName) {
        self.branch = Some(name);
    }
}

/// Ссылка на сессию опенкода: обязана иметь префикс `ses_`
/// (инвариант исполнителя; мусорные id не становятся заявками).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRef(String);

impl SessionRef {
    /// Валидирующий парсер: `ses_…` или None.
    pub fn parse(id: &str) -> Option<Self> {
        let id = id.trim();
        (id.starts_with("ses_") && id.len() > 4).then(|| Self(id.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Имя ветки задачи: непустое, без пробелов и glob-символов
/// (git-ref-безопасность: `..`, `~`, `^`, `:`, `?`, `*`, `[` запрещены).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchName(String);

impl BranchName {
    pub fn parse(name: &str) -> Option<Self> {
        let name = name.trim();
        let safe = !name.is_empty()
            && !name.contains(' ')
            && !["..", "~", "^", ":", "?", "*", "[", "\\"]
                .iter()
                .any(|bad| name.contains(bad))
            && !name.starts_with('-')
            && !name.ends_with(".lock");
        safe.then(|| Self(name.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// URL PR: только http(s) (канон PR-ячейки — кликабельная ссылка).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrUrl(String);

impl PrUrl {
    pub fn parse(url: &str) -> Option<Self> {
        let url = url.trim();
        (url.starts_with("https://") || url.starts_with("http://")).then(|| Self(url.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Агрегат: срез строки доски, достаточный конвейеру.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    number: u64,
    title: String,
    status: Status,
    priority: Priority,
    autodone: bool,
    fields: TaskFields,
}

impl Task {
    pub fn new(number: u64, title: String, status: Status) -> Self {
        Self {
            number,
            title,
            status,
            priority: Priority::Empty,
            autodone: false,
            fields: TaskFields::default(),
        }
    }

    /// Переход статуса с каноном рёбер (см. Status::can_transition):
    /// запрещённый ребро — громкая ошибка, мутации нет.
    pub fn transition(&mut self, to: Status) -> Result<(), String> {
        if !Status::can_transition(self.status, to) {
            return Err(format!(
                "переход «{}» → «{}» вне канона конвейера (см. Status::can_transition)",
                self.status.as_str(),
                to.as_str()
            ));
        }
        self.status = to;
        Ok(())
    }

    pub fn number(&self) -> u64 {
        self.number
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn status(&self) -> Status {
        self.status
    }

    pub fn priority(&self) -> Priority {
        self.priority
    }

    pub fn autodone(&self) -> bool {
        self.autodone
    }

    pub fn branch(&self) -> Option<&BranchName> {
        self.fields.branch()
    }

    pub fn pr(&self) -> Option<&PrUrl> {
        self.fields.pr()
    }

    /// Управление приоритетом (владелец/триаж).
    pub fn set_priority(&mut self, p: Priority) {
        self.priority = p;
    }

    /// Управление автодоном (владелец/триаж).
    pub fn set_autodone(&mut self, on: bool) {
        self.autodone = on;
    }

    /// Заявка сессии (claim): терминальная строка не принимает заявок —
    /// конвейер закончился, спавн поверх Done — ошибка.
    pub fn claim(&mut self, slot: Slot, session: SessionRef) -> Result<(), String> {
        if self.status.is_terminal() {
            return Err(format!(
                "claim на терминальной строке «{}» запрещён",
                self.status.as_str()
            ));
        }
        match slot {
            Slot::Dev | Slot::Design => self.fields.dev_session = Some(session),
            Slot::PlanReview | Slot::ResultReview => self.fields.review_session = Some(session),
            Slot::Test => self.fields.test_session = Some(session),
        }
        Ok(())
    }

    /// Сессия слота (None — не заявлена).
    pub fn session_of(&self, slot: Slot) -> Option<&str> {
        self.fields.session(slot).map(|s| s.as_str())
    }

    /// Слот стадии статуса: чья сессия ведёт строку сейчас.
    pub fn stage_slot(&self) -> Option<Slot> {
        match self.status {
            Status::Design => Some(Slot::Design),
            Status::ReviewPlan => Some(Slot::PlanReview),
            Status::InProgress => Some(Slot::Dev),
            Status::ReviewResult => Some(Slot::ResultReview),
            Status::Testing => Some(Slot::Test),
            _ => None,
        }
    }

    /// Стадии без сессии ждут спавна агента.
    pub fn stage_needs_spawn(&self) -> Option<Slot> {
        let slot = self.stage_slot()?;
        self.session_of(slot).is_none().then_some(slot)
    }

    /// Обогащение нативными полями (типизированы; лениво для кандидатов тика).
    pub fn apply_fields(&mut self, fields: TaskFields) {
        self.fields = fields;
    }

    /// Автодон-кандидат: Review + autodone.
    pub fn autodone_ready(&self) -> bool {
        self.status == Status::UserReview && self.autodone
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trip() {
        for raw in [
            "Draft",
            "Todo",
            "Design",
            "Ревью плана",
            "In Progress",
            "Ревью результата",
            "Тестирование",
            "Review",
            "Done",
            "Needs attention",
            "Вейт",
            "Deleted",
        ] {
            assert_eq!(Status::parse(raw).as_str(), raw, "round-trip {raw}");
        }
        assert_eq!(Status::parse("  In Progress "), Status::InProgress);
        assert_eq!(Status::parse(""), Status::Empty);
        assert_eq!(Status::parse("мусор"), Status::Empty);
    }

    #[test]
    fn active_and_terminal_sets() {
        assert!(Status::Design.is_active());
        assert!(Status::Testing.is_active());
        assert!(!Status::UserReview.is_active());
        assert!(Status::Done.is_terminal());
        assert!(Status::Deleted.is_terminal());
        assert!(!Status::NeedsAttention.is_active());
        assert!(!Status::NeedsAttention.is_terminal());
    }

    #[test]
    fn stage_slot_maps_status() {
        let mut t = Task::new(1, "t".into(), Status::ReviewPlan);
        assert_eq!(t.stage_slot(), Some(Slot::PlanReview));
        assert_eq!(t.stage_needs_spawn(), Some(Slot::PlanReview));
        t.claim(
            Slot::PlanReview,
            SessionRef::parse("ses_1").expect("валидная ссылка"),
        )
        .expect("claim рабочей стадии");
        assert_eq!(t.stage_needs_spawn(), None, "заявлена сессия — спавна нет");
        assert_eq!(t.session_of(Slot::PlanReview), Some("ses_1"));
    }

    #[test]
    fn claim_respects_slot_dedup() {
        let mut t = Task::new(2, "t".into(), Status::InProgress);
        t.claim(Slot::Dev, SessionRef::parse("ses_a").unwrap())
            .unwrap();
        t.claim(Slot::Dev, SessionRef::parse("ses_a").unwrap())
            .unwrap();
        assert_eq!(
            t.session_of(Slot::Dev),
            Some("ses_a"),
            "повторный claim той же сессии не плодит"
        );
    }

    #[test]
    fn claim_rejected_on_terminal() {
        let mut t = Task::new(3, "t".into(), Status::Done);
        assert!(t
            .claim(Slot::Dev, SessionRef::parse("ses_a").unwrap())
            .is_err());
    }

    #[test]
    fn value_objects_reject_garbage() {
        assert!(SessionRef::parse("мусор").is_none());
        assert!(SessionRef::parse("ses_").is_none());
        assert!(SessionRef::parse("ses_ok").is_some());
        assert!(BranchName::parse("").is_none());
        assert!(BranchName::parse("пробел в имени").is_none());
        assert!(BranchName::parse("work/task-1").is_some());
        assert!(PrUrl::parse("ftp://x").is_none());
        assert!(PrUrl::parse("https://github.com/x/pull/1").is_some());
    }

    #[test]
    fn status_transitions_canon() {
        use Status as S;
        // рёбра пайпа
        for (from, to) in [
            (S::Draft, S::Todo),
            (S::Todo, S::Design),
            (S::Todo, S::InProgress),
            (S::Design, S::ReviewPlan),
            (S::Design, S::InProgress),
            (S::InProgress, S::ReviewPlan),
            (S::InProgress, S::ReviewResult),
            (S::ReviewPlan, S::InProgress),
            (S::ReviewResult, S::Testing),
            (S::ReviewResult, S::InProgress),
            (S::Testing, S::UserReview),
            (S::Testing, S::InProgress),
            (S::UserReview, S::InProgress),
            (S::NeedsAttention, S::InProgress),
        ] {
            assert!(
                S::can_transition(from, to),
                "{from:?} → {to:?} должно быть разрешено"
            );
        }
        // запреты: Done не выходит, агенты не ставят Done/Todo/Draft через переход
        assert!(!S::can_transition(S::Done, S::InProgress));
        assert!(
            !S::can_transition(S::UserReview, S::Done),
            "Done — только close/автодон"
        );
        assert!(!S::can_transition(S::InProgress, S::Todo));
        assert!(
            !S::can_transition(S::Empty, S::Todo),
            "легаси-черновик — владельцу в UI"
        );
        // Needs attention — из любой рабочей стадии
        for from in [
            S::Design,
            S::ReviewPlan,
            S::InProgress,
            S::ReviewResult,
            S::Testing,
            S::UserReview,
        ] {
            assert!(
                S::can_transition(from, S::NeedsAttention),
                "{from:?} → Needs attention"
            );
        }
        // мутация с каноном
        let mut t = Task::new(9, "t".into(), S::InProgress);
        assert!(t.transition(S::ReviewResult).is_ok());
        assert!(
            t.transition(S::Done).is_err(),
            "ReviewResult → Done вне канона"
        );
        assert_eq!(
            t.status(),
            S::ReviewResult,
            "запрещённый переход не мутировал"
        );
    }

    #[test]
    fn autodone_requires_user_review_and_flag() {
        let mut t = Task::new(3, "t".into(), Status::UserReview);
        assert!(!t.autodone_ready());
        t.autodone = true;
        assert!(t.autodone_ready());
        t.status = Status::Testing;
        assert!(!t.autodone_ready());
    }

    #[test]
    fn priority_orders_queue() {
        assert!(Priority::High.rank() < Priority::Medium.rank());
        assert!(Priority::Low.rank() < Priority::Empty.rank());
    }
}
