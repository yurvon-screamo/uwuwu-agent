//! Агрегат Task: строка доски + сессии стадий + переходы статусов.
//! Чистый домен: ноль зависимостей. Инварианты канона — здесь и в тестах.

use crate::task_type::TaskType;

/// Статусы канона досок uwuwu. Analys/Review Analys — стадия аналитики
/// (входящий поток, замена триажа #1135). Empty — строка без статуса
/// (легаси-черновик владельца: конвейер не трогает).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Draft,
    Analys,
    ReviewAnalys,
    Todo,
    Design,
    ReviewPlan,
    InProgress,
    ReviewResult,
    Testing,
    /// Ревью тестового кода (#1413): после pass тестировщика с
    /// тестовыми коммитами — независимый взгляд на тест-дельту,
    /// которую код-ревью не видело (компромисс ADR-066).
    ReviewTesting,
    UserReview,
    Done,
    NeedsAttention,
    Wait,
    Deleted,
    Empty,
}

impl Status {
    /// LLM-тратящий статус: занимает слот CUP.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Design
                | Self::ReviewPlan
                | Self::InProgress
                | Self::ReviewResult
                | Self::Testing
                | Self::ReviewTesting
        )
    }

    /// Терминальные: конвейер больше не трогает (Deleted — мягкое удаление).
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Deleted)
    }

    /// Канон переходов (AGENTS.md «статусы кто ставит» + пайп конвейера):
    /// рёбра пайплайна + Needs attention из любой рабочей стадии; Done —
    /// только через close владельца (не task_edit); Done/Deleted/Empty —
    /// без исходящих рёбер (реопен/легаси — владелец в UI, мимо MCP).
    /// Fast-track (#1135, развилка 6): Draft/Analys/Todo → In Progress —
    /// владелец тащит простые задачи сразу в работу.
    pub fn can_transition(from: Self, to: Self) -> bool {
        use Status as S;
        if from == to {
            return true; // идемпотентная переустановка
        }
        // Возврат в Вейт открыт из рабочих/финальных статусов: крон-мастер
        // после правок тела обязан вернуться в Вейт (файр-гейт пускает только
        // из него; унификация кронов с задачами забыла это ребро — мастер
        // #384 застрял в In Progress, расписание умерло). Из Draft Вейт не
        // ставится: черновики в Вейт проводит триаж.
        let to_wait = matches!(to, S::Wait)
            && matches!(
                from,
                S::Todo
                    | S::Design
                    | S::ReviewPlan
                    | S::InProgress
                    | S::ReviewResult
                    | S::Testing
                    | S::ReviewTesting
                    | S::UserReview
                    | S::NeedsAttention
            );
        if to_wait {
            return true;
        }
        matches!(
            (from, to),
            (S::Draft, S::Todo | S::Analys | S::InProgress | S::Deleted)
                | (
                    S::Analys,
                    S::ReviewAnalys | S::InProgress | S::NeedsAttention | S::Deleted
                )
                | (S::ReviewAnalys, S::Todo | S::Analys | S::NeedsAttention)
                | (S::Todo, S::Design | S::InProgress | S::Analys | S::Deleted)
                | (S::Wait, S::Todo | S::Design | S::InProgress | S::Analys)
                | (S::Design, S::ReviewPlan | S::InProgress | S::NeedsAttention)
                | (S::ReviewPlan, S::Design | S::InProgress | S::NeedsAttention)
                | (
                    S::InProgress,
                    S::ReviewPlan | S::ReviewResult | S::Testing | S::NeedsAttention
                )
                | (
                    S::ReviewResult,
                    S::InProgress | S::Testing | S::NeedsAttention
                )
                | (
                    S::Testing,
                    S::UserReview | S::ReviewTesting | S::InProgress | S::NeedsAttention
                )
                | (
                    // Ревью тестирования (#1413): вердикт тест-ревьювера —
                    // чисто в Review, находки назад тестировщику (пара
                    // счёта капа #1311), вопрос владельцу. Прод-код в
                    // In Progress не возвращается — fail-цикл тестировщика.
                    S::ReviewTesting,
                    S::UserReview | S::Testing | S::NeedsAttention
                )
                | (S::UserReview, S::InProgress | S::NeedsAttention)
                | (S::NeedsAttention, S::InProgress | S::Analys)
        )
    }

    /// Канон + direct-класс (#1357): единственное новое ребро
    /// In Progress → Review открыто строке с флагом direct (прямое
    /// поручение владельца — «сразу в ревью»). Обычные строки финализирует
    /// ревью-конвейер (Ревью результата → Тестирование → Review); базовый
    /// `can_transition` и его пин-тесты не тронуты.
    pub fn can_transition_with(from: Self, to: Self, direct: bool) -> bool {
        use Status as S;
        (direct && from == S::InProgress && to == S::UserReview) || Self::can_transition(from, to)
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
    TestReview,
}

impl Slot {
    /// Эмодзи-маркер стадии в тайтле спавн-сессии (задача #1212).
    /// Design/Dev == 🔨 осознанно: оба слота — дефолтная сессия
    /// исполнителя; равенство с Agent::Developer.emoji() связано тестом.
    pub fn emoji(self) -> &'static str {
        match self {
            Self::Design | Self::Dev => "🔨",
            Self::PlanReview => "🔍",
            Self::ResultReview => "🔬",
            Self::Test => "🧪",
            Self::TestReview => "🧫",
        }
    }
}

/// Нативные поля issue, типизированные и инкапсулированные:
/// домен владеет мутациями, инфра маппит имена → конструкторы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFields {
    /// Назначения сессий по слотам: отсутствие слота = назначения нет
    /// (явная пустота коллекции, не Option-поле).
    assignments: std::collections::BTreeMap<Slot, SessionRef>,
    /// Жизненный цикл VCS-артефактов: явные состояния вместо Option-полей.
    vcs: Vcs,
    /// Приоритет с носителя: гейт пика пропускает High в окно GLM
    /// (канон «high продолжают работать»; инцидент #1170 — приоритет
    /// не мапился вовсе, пик морозил даже high).
    priority: Priority,
}

/// VCS-состояние задачи: без ветки → ветка → PR. Кодовый инвариант:
/// PullRequest не бывает без ветки (мутаторы требуют Branch-ячейку).
/// Безветочная PR-ячейка — Artifact: URL-артефакт по канону ADR-021
/// (вики/issue/чужой-вендор PR) — рендерится в fm без Branch (#1155).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Vcs {
    Local,
    Branch(BranchName),
    PullRequest {
        branch: BranchName,
        url: PrUrl,
    },
    /// PR-ячейка несёт URL без ветки кода (docs-only артефакт, чужой
    /// вендор) — fm рендерит pr, branch остаётся пустым.
    Artifact {
        url: PrUrl,
    },
}

impl Default for TaskFields {
    fn default() -> Self {
        Self {
            assignments: std::collections::BTreeMap::new(),
            vcs: Vcs::Local,
            priority: Priority::Empty,
        }
    }
}

impl TaskFields {
    /// Полная сборка (загрузчик носителя).
    pub fn new(
        assignments: std::collections::BTreeMap<Slot, SessionRef>,
        vcs: Vcs,
        priority: Priority,
    ) -> Self {
        Self {
            assignments,
            vcs,
            priority,
        }
    }

    /// Приоритет с носителя (гейт пика читает его).
    pub fn priority(&self) -> Priority {
        self.priority
    }

    /// Сессия слота (None — слот не назначен: явная пустота карты).
    pub fn session(&self, slot: Slot) -> Option<&SessionRef> {
        self.assignments.get(&slot)
    }

    pub fn vcs(&self) -> &Vcs {
        &self.vcs
    }

    /// Ветка, если есть.
    pub fn branch(&self) -> Option<&BranchName> {
        match &self.vcs {
            Vcs::Local | Vcs::Artifact { .. } => None,
            Vcs::Branch(b) => Some(b),
            Vcs::PullRequest { branch, .. } => Some(branch),
        }
    }

    /// PR, если есть (включая безветочный артефакт — fm рендерит оба вида).
    pub fn pr(&self) -> Option<&PrUrl> {
        match &self.vcs {
            Vcs::PullRequest { url, .. } | Vcs::Artifact { url } => Some(url),
            Vcs::Local | Vcs::Branch(_) => None,
        }
    }

    /// Мутатор: заявка сессии в слот (инфра-маппинг полей и спавны).
    pub fn set_session(&mut self, slot: Slot, session: SessionRef) {
        self.assignments.insert(slot, session);
    }

    /// Снятие сессии слота (раунд свежих глаз: ревью/тест не переживают
    /// смену стадии — канон сессий).
    pub fn clear_session(&mut self, slot: Slot) {
        self.assignments.remove(&slot);
    }

    /// Мутатор: заявка/смена ветки; открытый PR сохраняется (ветка задачи
    /// одна — канон); Artifact вытесняется веткой кода.
    pub fn set_branch(&mut self, name: BranchName) {
        self.vcs = match std::mem::replace(&mut self.vcs, Vcs::Local) {
            Vcs::PullRequest { url, .. } => Vcs::PullRequest { branch: name, url },
            _ => Vcs::Branch(name),
        };
    }

    /// Мутатор: безветочная PR-ячейка — URL-артефакт (канон ADR-021:
    /// «PR-ячейка заполняется всегда, включая docs-only»).
    pub fn set_artifact(&mut self, url: PrUrl) {
        self.vcs = Vcs::Artifact { url };
    }

    /// Мутатор: открытие PR (требуется ветка — канон: PR из ветки задачи).
    pub fn open_pr(&mut self, url: PrUrl) -> Result<(), &'static str> {
        match self.vcs.clone() {
            Vcs::Local | Vcs::Artifact { .. } => Err("PR без ветки: сначала Branch-ячейка"),
            Vcs::Branch(branch) => {
                self.vcs = Vcs::PullRequest { branch, url };
                Ok(())
            }
            Vcs::PullRequest { branch, .. } => {
                self.vcs = Vcs::PullRequest { branch, url };
                Ok(())
            }
        }
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

    /// Грубая PR-форма URL: хвост пути `/pull/<n>` или `/pulls/<n>`
    /// (GitHub/GHE и Gitea/Forgejo семейства), хвостовой `/` допустим,
    /// хост не важен. Домен отвечает только «это вообще PR-форма?»; вендор
    /// и строгость (literal github.com, чужие сущности по числовым
    /// сегментам) — адаптер в своём parse_pr_url (#1155). Query/fragment и
    /// нецифровой хвост — false: гейт не пускает то, что строгий слой
    /// отвергнет (#1286).
    pub fn is_pull(&self) -> bool {
        let Some((_, rest)) = self.0.split_once("://") else {
            return false;
        };
        let path = rest.trim_end_matches('/');
        let Some((head, tail)) = path.rsplit_once('/') else {
            return false;
        };
        if !head.ends_with("/pull") && !head.ends_with("/pulls") {
            return false;
        }
        !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit())
    }
}

/// Агрегат: срез строки доски, достаточный конвейеру.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    number: u64,
    title: String,
    status: Status,
    priority: Priority,
    automerge: bool,
    /// Прямое поручение владельца (#1357): строка рождается сразу в
    /// In Progress и финализируется агентом в Review, мимо фабричных
    /// стадий. По образцу automerge: пишется на носитель явно (create),
    /// читается с колонки доски.
    direct: bool,
    /// Тип задачи (нативный issue Type — единственный носитель, #1271):
    /// None — не задан (native-null строки, неизвестное имя, дефолт Task).
    kind: Option<TaskType>,
    /// Дата закрытия issue (unix ms): None — issue открыт либо носитель
    /// дату не сообщает. Клин холодного ящика (#1293) отбирает
    /// терминальные строки по возрасту закрытия.
    closed_at: Option<u64>,
    fields: TaskFields,
}

impl Task {
    pub fn new(number: u64, title: String, status: Status) -> Self {
        Self {
            number,
            title,
            status,
            priority: Priority::Empty,
            automerge: false,
            direct: false,
            kind: None,
            closed_at: None,
            fields: TaskFields::default(),
        }
    }

    /// Переход статуса с каноном рёбер (см. Status::can_transition):
    /// запрещённый ребро — громкая ошибка, мутации нет. direct-строка
    /// (#1357) несёт собственный флаг — гвард через can_transition_with.
    pub fn transition(&mut self, to: Status) -> Result<(), String> {
        if !Status::can_transition_with(self.status, to, self.direct) {
            return Err(format!(
                "переход {:?} → {:?} вне канона конвейера (см. Status::can_transition)",
                self.status, to
            ));
        }
        self.status = to;
        Ok(())
    }

    /// Запись известного значения носителя мимо канона: write-through
    /// кэша доски после успешной мутации (#1235). Канон-гард лежит в
    /// usecase edit(); здесь значение только фиксируется — носитель
    /// уже принял его.
    pub fn override_status(&mut self, to: Status) {
        self.status = to;
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

    pub fn automerge(&self) -> bool {
        self.automerge
    }

    /// Флаг direct (create-only, #1357): строка прямого поручения.
    pub fn direct(&self) -> bool {
        self.direct
    }

    /// Тип задачи (нативный issue Type, #1271): None — не задан.
    pub fn kind(&self) -> Option<TaskType> {
        self.kind
    }

    /// Запись типа с носителя (parse issue Type) или через edit (#1271).
    pub fn set_kind(&mut self, kind: Option<TaskType>) {
        self.kind = kind;
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

    /// Управление автомержем (владелец/триаж).
    pub fn set_automerge(&mut self, on: bool) {
        self.automerge = on;
    }

    /// Запись флага direct с носителя (parse колонки доски).
    pub fn set_direct(&mut self, on: bool) {
        self.direct = on;
    }

    /// Дата закрытия issue (unix ms): None — открыт либо не сообщена.
    pub fn closed_at(&self) -> Option<u64> {
        self.closed_at
    }

    /// Запись даты закрытия с носителя (парсер снапшота).
    pub fn set_closed_at(&mut self, ms: u64) {
        self.closed_at = Some(ms);
    }

    /// Кандидат клина холодного ящика (#1293): строка терминальна
    /// (конвейер закончился) и issue закрыт строго старше `age_days`
    /// суток. Дата закрытия = доказательство закрытого issue; строка
    /// без даты — пропуск, консервативно.
    pub fn is_sweep_candidate(&self, now_ms: u64, age_days: u32) -> bool {
        if !self.status.is_terminal() {
            return false;
        }
        let Some(closed) = self.closed_at else {
            return false;
        };
        now_ms.saturating_sub(closed) > u64::from(age_days) * 86_400_000
    }

    /// Заявка сессии (claim): терминальная строка не принимает заявок —
    /// конвейер закончился, спавн поверх Done — ошибка.
    /// Снятие сессии слота самой строкой (тик после clear_session доски —
    /// локальный клон должен видеть свежие глаза).
    pub fn clear_slot_session(&mut self, slot: Slot) {
        self.fields.clear_session(slot);
    }

    pub fn claim(&mut self, slot: Slot, session: SessionRef) -> Result<(), String> {
        if self.status.is_terminal() {
            return Err(format!(
                "claim на терминальной строке ({:?}) запрещён",
                self.status
            ));
        }
        self.fields.set_session(slot, session);
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
            Status::ReviewTesting => Some(Slot::TestReview),
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
        // приоритет — поле самой задачи; Empty на носителе = данных нет,
        // существующий приоритет (snapshot-колонка доски) не стирается:
        // перезапись Empty-ом сбрасывала high → пик морозил высокую строку
        if fields.priority != Priority::Empty {
            self.priority = fields.priority;
        }
        self.fields = fields;
    }

    /// Автомерж-кандидат (#1232): Review + automerge + кодовый носитель
    /// (ветка/PR). Безветочная ячейка (Local и Artifact — docs-only
    /// артефакт/чужой-вендор URL, #1155) мержить нечего — строка ждёт
    /// владельца в Review, тик её не прощупывает. PR-ячейка с URL не
    /// PR-формы (wiki/issues/blob — docs-only с фабричной веткой, #1286)
    /// — тот же класс «нечего мержить»: гейт требует PR-формы, когда URL
    /// есть; без URL (ветка одна) кандидат остаётся — веточная проба,
    /// авторемонт UC-01 (PR ещё не создан).
    pub fn automerge_ready(&self) -> bool {
        self.status == Status::UserReview
            && self.automerge
            && matches!(self.fields.vcs(), Vcs::Branch(_) | Vcs::PullRequest { .. })
            && self.fields.pr().map(PrUrl::is_pull).unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;

    fn task(status: Status) -> Task {
        Task::new(7, "t".into(), status)
    }

    /// Маркеры слотов попарно различимы, кроме осознанной пары
    /// Design/Dev (оба 🔨); связка слотов с ролями — ассертом равенства
    /// (критерий различимости #1212).
    #[test]
    fn slot_emojis_distinct_and_linked_to_agents() {
        let emojis: [&str; 6] = [
            Slot::Design.emoji(),
            Slot::Dev.emoji(),
            Slot::PlanReview.emoji(),
            Slot::ResultReview.emoji(),
            Slot::Test.emoji(),
            Slot::TestReview.emoji(),
        ];
        for i in 0..emojis.len() {
            for j in i + 1..emojis.len() {
                if i == 0 && j == 1 {
                    continue; // Design/Dev — осознанное совпадение
                }
                assert_ne!(emojis[i], emojis[j]);
            }
        }
        assert_eq!(emojis[0], emojis[1], "Design == Dev == 🔨");
        assert_eq!(emojis[1], Agent::Developer.emoji());
        assert_eq!(emojis[2], Agent::PlanReviewer.emoji());
        assert_eq!(emojis[3], Agent::CodeQualityReviewer.emoji());
        assert_eq!(emojis[4], Agent::Tester.emoji());
        assert_eq!(emojis[5], Agent::TestReviewer.emoji());
    }

    #[test]
    fn review_testing_stage_is_wired() {
        // #1413: статус → слот → спавн; is_active — CUP/пик
        let t = task(Status::ReviewTesting);
        assert_eq!(t.stage_slot(), Some(Slot::TestReview));
        assert_eq!(t.stage_needs_spawn(), Some(Slot::TestReview));
        assert!(
            Status::ReviewTesting.is_active(),
            "стадия LLM-тратящая — слот CUP и пик-гейт"
        );
        assert!(
            !Status::ReviewTesting.is_terminal(),
            "рабочая стадия, не терминал"
        );
    }

    #[test]
    fn claim_respects_terminal() {
        let mut t = Task::new(1, "t".into(), Status::Done);
        assert!(t
            .claim(Slot::Dev, SessionRef::parse("ses_a").unwrap())
            .is_err());
        let mut live = Task::new(1, "t".into(), Status::InProgress);
        assert!(live
            .claim(Slot::Dev, SessionRef::parse("ses_a").unwrap())
            .is_ok());
        assert_eq!(live.session_of(Slot::Dev), Some("ses_a"));
    }

    #[test]
    fn stage_slot_maps_status() {
        let mut t = task(Status::ReviewPlan);
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
    fn is_pull_coarse_form() {
        // #1286: грубая форма — хвост пути /pull/<n>, хост не важен
        let pull = |url: &str| PrUrl::parse(url).unwrap().is_pull();
        assert!(pull("https://github.com/o/r/pull/7"));
        assert!(
            pull("https://github.com/o/r/pull/7/"),
            "трейлинг-слэш не рвёт форму"
        );
        assert!(pull("https://ghe.corp/o/r/pull/7"), "хост не важен");
        // артефакт-URL (wiki/issues/blob) — не PR-форма
        assert!(!pull("https://github.com/o/r/wiki/a"));
        assert!(!pull("https://github.com/o/r/issues/1279"));
        assert!(!pull("https://github.com/o/r/blob/main/docs/x.md"));
        // #1269: Gitea /pulls/<n> — вторая грубая форма (Gitea/Forgejo
        // семейство): гейт пускает, вендор-разбор — в адаптере
        assert!(pull("https://git.l4a.ru/o/r/pulls/7"), "#1269: Gitea-форма");
        // мусорные хвосты: строгий слой их отвергнет — гейт не пускает
        assert!(!pull("https://github.com/o/r/pull/"), "пустой номер");
        assert!(!pull("https://github.com/o/r/pull/abc"), "нецифровой номер");
        assert!(!pull("https://github.com/o/r/pull/7#x"), "fragment");
        assert!(!pull("https://github.com/o/r/pull"), "без номера");
    }

    #[test]
    fn vcs_lifecycle_explicit() {
        let mut f = TaskFields::default();
        assert!(matches!(f.vcs(), &Vcs::Local), "нет ветки — Local");
        assert!(f.pr().is_none());
        // PR без ветки — ошибка домена
        assert!(f.open_pr(PrUrl::parse("https://x/1").unwrap()).is_err());
        f.set_branch(BranchName::parse("work/task-1").unwrap());
        assert!(matches!(f.vcs(), &Vcs::Branch(_)));
        f.open_pr(PrUrl::parse("https://x/1").unwrap()).unwrap();
        match f.vcs() {
            Vcs::PullRequest { branch, url } => {
                assert_eq!(branch.as_str(), "work/task-1");
                assert_eq!(url.as_str(), "https://x/1");
            }
            other => panic!("PR ожидался, got {other:?}"),
        }
        // смена ветки сохраняет PR
        f.set_branch(BranchName::parse("work/task-1b").unwrap());
        assert!(f.pr().is_some());
        assert_eq!(f.branch().map(|b| b.as_str()), Some("work/task-1b"));
    }

    #[test]
    fn artifact_url_renders_pr_without_branch() {
        // #1155: безветочная PR-ячейка — Artifact, fm рендерит pr, branch пуст
        let mut f = TaskFields::default();
        f.set_artifact(PrUrl::parse("https://github.com/x/y/blob/main/docs/a.md").unwrap());
        assert!(matches!(f.vcs(), Vcs::Artifact { .. }));
        assert_eq!(
            f.pr().map(|u| u.as_str()),
            Some("https://github.com/x/y/blob/main/docs/a.md")
        );
        assert!(f.branch().is_none());
    }

    #[test]
    fn open_pr_from_artifact_requires_branch() {
        // инвариант кодового цикла: PR без ветки — ошибка и из Artifact
        let mut f = TaskFields::default();
        f.set_artifact(PrUrl::parse("https://x/1").unwrap());
        assert!(f.open_pr(PrUrl::parse("https://x/2").unwrap()).is_err());
    }

    #[test]
    fn set_branch_displaces_artifact() {
        let mut f = TaskFields::default();
        f.set_artifact(PrUrl::parse("https://x/1").unwrap());
        f.set_branch(BranchName::parse("work/task-1").unwrap());
        assert!(
            matches!(f.vcs(), Vcs::Branch(_)),
            "ветка вытесняет артефакт"
        );
        assert!(f.pr().is_none());
        f.open_pr(PrUrl::parse("https://x/2").unwrap()).unwrap();
        assert!(matches!(f.vcs(), Vcs::PullRequest { .. }));
    }

    #[test]
    fn set_artifact_replaces_url() {
        let mut f = TaskFields::default();
        f.set_artifact(PrUrl::parse("https://x/1").unwrap());
        f.set_artifact(PrUrl::parse("https://x/2").unwrap());
        assert_eq!(f.pr().map(|u| u.as_str()), Some("https://x/2"));
    }

    #[test]
    fn status_transitions_canon() {
        use Status as S;
        // рёбра пайпа
        for (from, to) in [
            (S::Draft, S::Todo),
            (S::Draft, S::Deleted),
            (S::Todo, S::Design),
            (S::Todo, S::InProgress),
            (S::Todo, S::Deleted),
            (S::Design, S::ReviewPlan),
            (S::Design, S::InProgress),
            (S::Design, S::NeedsAttention),
            (S::ReviewPlan, S::Design),
            (S::ReviewPlan, S::InProgress),
            (S::InProgress, S::ReviewPlan),
            (S::InProgress, S::ReviewResult),
            // прямая отправка на тест (#1349): дев-агент из In Progress
            (S::InProgress, S::Testing),
            (S::ReviewResult, S::Testing),
            (S::ReviewResult, S::InProgress),
            (S::Testing, S::UserReview),
            (S::Testing, S::InProgress),
            // ревью тестирования (#1413): pass с тест-коммитами и вердикты
            (S::Testing, S::ReviewTesting),
            (S::ReviewTesting, S::UserReview),
            (S::ReviewTesting, S::Testing),
            (S::ReviewTesting, S::NeedsAttention),
            (S::UserReview, S::InProgress),
            (S::NeedsAttention, S::InProgress),
            // аналитика (#1135): вход потока и цикл с ревью
            (S::Draft, S::Analys),
            (S::Analys, S::ReviewAnalys),
            (S::Analys, S::NeedsAttention),
            (S::Analys, S::Deleted),
            (S::ReviewAnalys, S::Todo),
            (S::ReviewAnalys, S::Analys),
            (S::ReviewAnalys, S::NeedsAttention),
            (S::Todo, S::Analys),
            (S::Wait, S::Analys),
            (S::NeedsAttention, S::Analys),
            // fast-track (развилка 6): Draft/Analys/Todo → In Progress
            (S::Draft, S::InProgress),
            (S::Analys, S::InProgress),
        ] {
            assert!(
                S::can_transition(from, to),
                "{from:?} → {to:?} должно быть разрешено"
            );
        }
        // запреты
        assert!(!S::can_transition(S::Done, S::InProgress));
        assert!(
            !S::can_transition(S::UserReview, S::Done),
            "Done — только close владельца"
        );
        assert!(!S::can_transition(S::InProgress, S::Todo));
        assert!(
            !S::can_transition(S::ReviewTesting, S::InProgress),
            "тест-ревьювер прод-код не правит — fail-цикл тестировщика"
        );
        assert!(
            !S::can_transition(S::ReviewTesting, S::ReviewResult),
            "назад в код-ревью тест-дельта не ходит"
        );
        assert!(
            !S::can_transition(S::Empty, S::Todo),
            "легаси — владельцу в UI"
        );
        assert!(
            !S::can_transition(S::InProgress, S::Deleted),
            "в работе — не удаляют"
        );
        // аналитика: цикл замкнут, мимо ревью в пайп не пройти
        assert!(
            !S::can_transition(S::Analys, S::Todo),
            "Todo — вердикт ревьювера"
        );
        assert!(
            !S::can_transition(S::Analys, S::Design),
            "Design — через Todo (мяч владельца)"
        );
        assert!(
            !S::can_transition(S::Draft, S::ReviewAnalys),
            "Review Analys ставит только аналитик"
        );
        assert!(
            !S::can_transition(S::ReviewAnalys, S::InProgress),
            "fast-track — из очереди и Todo, не из ревью аналитики"
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
        assert!(t.transition(S::Done).is_err());
        assert_eq!(
            t.status(),
            S::ReviewResult,
            "запрещённый переход не мутировал"
        );
    }

    #[test]
    fn direct_review_edge_is_flag_gated() {
        use Status as S;
        // direct-ребро (#1357): In Progress → Review только с флагом
        assert!(
            S::can_transition_with(S::InProgress, S::UserReview, true),
            "direct-строка финализируется агентом сам"
        );
        assert!(
            !S::can_transition_with(S::InProgress, S::UserReview, false),
            "без флага — «Ревью результата» обязателен (канон)"
        );
        // флаг не открывает лишнего: остальные прямые — по канону
        assert!(!S::can_transition_with(S::InProgress, S::Done, true));
        assert!(!S::can_transition_with(S::Analys, S::UserReview, true));
        // мутация на агрегате: флаг default false, сеттер пишет
        let mut t = Task::new(7, "direct-строка".into(), S::InProgress);
        assert!(!t.direct(), "по умолчанию строка не direct");
        assert!(t.transition(S::UserReview).is_err());
        t.set_direct(true);
        assert!(t.transition(S::UserReview).is_ok());
        assert_eq!(t.status(), S::UserReview);
    }

    #[test]
    fn wait_return_edges() {
        use Status as S;
        // Крон-мастер после правок тела обязан вернуться в Вейт (файр-гейт
        // пускает только из него) — ребро возврата открыто из рабочих и
        // финальных статусов (инцидент #384: мастер застрял в In Progress,
        // расписание умерло). Из Draft Вейт не ставится — это рука триажа.
        for from in [
            S::Todo,
            S::Design,
            S::ReviewPlan,
            S::InProgress,
            S::ReviewResult,
            S::Testing,
            S::UserReview,
            S::NeedsAttention,
        ] {
            assert!(
                S::can_transition(from, S::Wait),
                "{from:?} → Wait обязана быть"
            );
        }
        assert!(
            !S::can_transition(S::Draft, S::Wait),
            "Draft → Wait — триаж"
        );
        assert!(S::can_transition(S::Wait, S::Wait), "идемпотентность");
    }

    #[test]
    fn apply_fields_replaces_whole() {
        let mut t = task(Status::ReviewResult);
        let mut assignments = std::collections::BTreeMap::new();
        assignments.insert(Slot::PlanReview, SessionRef::parse("ses_r").unwrap());
        let fields = TaskFields::new(
            assignments,
            Vcs::Branch(BranchName::parse("work/task-3").unwrap()),
            Priority::Empty,
        );
        t.apply_fields(fields);
        assert_eq!(t.session_of(Slot::PlanReview), Some("ses_r"));
        assert_eq!(t.branch().map(|b| b.as_str()), Some("work/task-3"));
        assert_eq!(t.priority(), Priority::Empty);
    }

    #[test]
    fn apply_fields_carries_priority() {
        // #1170: приоритет с носителя обязан доезжать до задачи — гейт пика
        // пропускает High в окно GLM (канон «high работают в пик»)
        let mut t = task(Status::ReviewResult);
        let fields = TaskFields::new(
            std::collections::BTreeMap::new(),
            Vcs::Local,
            Priority::High,
        );
        t.apply_fields(fields);
        assert_eq!(t.priority(), Priority::High);
    }

    #[test]
    fn apply_fields_keeps_priority_when_carrier_empty() {
        // Empty на носителе = данных нет: приоритет snapshot-колонки не
        // стирается (перезапись Empty-ом сбрасывала high → пик морозил
        // высокую строку — инцидент-семья #1200)
        let mut t = task(Status::ReviewResult);
        t.set_priority(Priority::High);
        t.apply_fields(TaskFields::new(
            std::collections::BTreeMap::new(),
            Vcs::Local,
            Priority::Empty,
        ));
        assert_eq!(t.priority(), Priority::High);
    }

    #[test]
    fn kind_defaults_to_none_and_set_kind_carries() {
        // #1271: тип — нативный issue Type; fresh-строка без типа (native
        // null, неизвестное имя, дефолт Task) читается None без ошибки
        let mut t = task(Status::Analys);
        assert_eq!(t.kind(), None);
        t.set_kind(Some(TaskType::Refactoring));
        assert_eq!(t.kind(), Some(TaskType::Refactoring));
        // снос значения носителем (PATCH type=null) — легальная мутация
        t.set_kind(None);
        assert_eq!(t.kind(), None);
    }

    const DAY_MS: u64 = 86_400_000;

    #[test]
    fn sweep_candidate_strictly_older_than_n_days() {
        // Граница «строго старше N суток» (#1293): ровно N суток — ещё нет,
        // N суток + миллисекунда — да
        let now = 1_800_000_000_000;
        let mut done = task(Status::Done);
        done.set_closed_at(now - 7 * DAY_MS);
        assert!(!done.is_sweep_candidate(now, 7), "ровно 7 суток — свежая");
        done.set_closed_at(now - (7 * DAY_MS + 1));
        assert!(
            done.is_sweep_candidate(now, 7),
            "чуть старше 7 суток — кандидат"
        );
    }

    #[test]
    fn sweep_candidate_requires_terminal_status_and_date() {
        let now = 1_800_000_000_000;
        let old = now - 30 * DAY_MS;
        // Активная/нетерминальная строка — не кандидат даже при закрытом issue
        let mut active = task(Status::UserReview);
        active.set_closed_at(old);
        assert!(!active.is_sweep_candidate(now, 7));
        // Терминальная без даты закрытия (древний Deleted, рассинхрон) — пропуск
        let deleted = task(Status::Deleted);
        assert!(!deleted.is_sweep_candidate(now, 7));
        // Терминальная с датой — кандидат
        let mut done = task(Status::Done);
        done.set_closed_at(old);
        assert!(done.is_sweep_candidate(now, 7));
        // Deleted с датой — тоже (мягкое удаление уходит в архив так же)
        let mut soft = task(Status::Deleted);
        soft.set_closed_at(old);
        assert!(soft.is_sweep_candidate(now, 7));
    }
}
