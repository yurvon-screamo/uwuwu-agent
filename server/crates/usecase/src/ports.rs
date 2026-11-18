//! Порты юзкейс-слоя: требования к инфре. Сигнатуры — на доменных типах
//! и нативных ошибках (UsecaseError); инфра (uwuwu-server) реализует их
//! поверх gh/опенкода/ФС.

use crate::error::UsecaseResult;
use async_trait::async_trait;
use uwuwu_domain::{AgentSlot, BranchName, ModelRef, Slot, Status, Task, TaskFields, UtcMinute};

/// Мутатор fm-ячеек задачи (branch/pr/priority/…).
pub type FmMutator =
    Box<dyn for<'a> FnOnce(&'a mut std::collections::BTreeMap<String, String>) + Send>;

/// Доска задач: чтение строк + мутации носителя (поля+fm синхронны —
/// инвариант «fm ≡ поля» держит реализация).
#[async_trait]
pub trait TaskBoardRepo: Send + Sync {
    async fn read_board(&self, board: &str) -> UsecaseResult<Vec<Task>>;
    async fn set_status(&self, board: &str, number: u64, status: Status) -> UsecaseResult<()>;
    /// Свежий статус строки с носителя, мимо кэшей доски — гард канона
    /// edit() обязан сверяться с носителем, не со снапшотом (#1235).
    /// None — Status-значения на носителе нет (строки нет на доске /
    /// поле не задано); Some(Empty) — значение носителя непарсящееся.
    async fn row_status(&self, board: &str, number: u64) -> UsecaseResult<Option<Status>>;
    /// Заявка сессии в слот (claim/спавн-привязка).
    async fn set_session(
        &self,
        board: &str,
        number: u64,
        slot: Slot,
        session: &str,
    ) -> UsecaseResult<()>;

    /// Заявка ветки задачи (фабрика при спавне: task-env создал env,
    /// ветка `work/task-<row>` существует — ячейка правдива; ADR-041).
    async fn set_branch(&self, board: &str, number: u64, branch: &BranchName) -> UsecaseResult<()>;

    /// Запись нестатусных ячеек снапшота доски (Приоритет, Automerge):
    /// поле + кеш-гигиена одним методом (паттерн set_session/set_branch).
    /// Контракт read-your-writes (#1258): после записи read_board отражает
    /// записанные значения сразу — write-through кешированного снапшота
    /// при живой строке, инвалидация при отсутствующей (окно create).
    /// None — ячейку не трогаем. Ветка/PR/Тип живут мимо снапшота —
    /// идут через IssueFieldStore напрямую.
    async fn set_board_cells(
        &self,
        board: &str,
        number: u64,
        priority: Option<&str>,
        automerge: Option<bool>,
    ) -> UsecaseResult<()>;

    /// Очистить сессию слота строки (переход в раунд свежих глаз:
    /// ревью/тест-сессии не переживают смену стадии — канон сессий).
    async fn clear_session(&self, board: &str, number: u64, slot: Slot) -> UsecaseResult<()>;
    /// Опубликовать коммент; возвращает node_id созданного коммента
    /// (пустая строка — id недоступен: маркировка реестра невозможна,
    /// self-эхо-фильтр деградирует честно — fail-open).
    async fn post_comment(&self, number: u64, body: &str) -> UsecaseResult<String>;
    /// Закрыть issue (Done по канону = closed).
    async fn close_issue(&self, number: u64) -> UsecaseResult<()>;
    /// Хвост комментов задачи (для wakeup: новые против последнего).
    async fn latest_comments(&self, number: u64) -> UsecaseResult<Vec<CommentRef>>;
    /// Полная карта задачи: агрегат + тело issue + все комменты.
    async fn read_issue(&self, board: &str, number: u64) -> UsecaseResult<IssueView>;
    /// Дешёвое обогащение: только нативные поля issue (сессии/ветка/PR/
    /// приоритет/тип) одним REST-зовом — без тела и повторного snapshot
    /// доски. Snapshot Projects v2 глобальных полей issue не несёт:
    /// без enrichment тик не видит сессии стадии (wakeup недостижим,
    /// needs_spawn вечен — инцидент ночи 15-16.11).
    async fn issue_fields(&self, number: u64) -> UsecaseResult<TaskFields>;
    /// Создание issue (лейбл proj:<project>) + строка на доске board
    /// (Analys — create-дефолт #1135; опция ставится по имени колонки).
    async fn create_issue(
        &self,
        board: &str,
        project: &str,
        title: &str,
        body: &str,
    ) -> UsecaseResult<u64>;
    /// Переименование (тайтл уникален — дубль отвергается юзкейсом выше).
    async fn rename_issue(&self, number: u64, title: &str) -> UsecaseResult<()>;
    /// Точечная правка fm-ячеек (branch/pr/priority/…) с сохранением контента.
    async fn patch_cells(&self, number: u64, mutate: FmMutator) -> UsecaseResult<()>;
    /// Закрытие с причиной (completed|not_planned).
    async fn close_issue_ext(&self, number: u64, not_planned: bool) -> UsecaseResult<()>;
    /// Полная замена тела issue (как записано; fm-шапка стрипается на
    /// границе юзкейса — тела без fm, поля нативные).
    async fn replace_body(&self, number: u64, body_md: &str) -> UsecaseResult<()>;
}

/// Задача целиком: для карточки task_get (агентские чтения).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueView {
    pub task: Task,
    pub body: String,
    pub comments: Vec<CommentRef>,
}

/// Вики-репозиторий: чтение/запись доков (MCP docs_*, кроны, дример).
#[async_trait]
pub trait WikiRepo: Send + Sync {
    /// markdown дока по слагу (docs/** без .md); None — не найден.
    async fn read_doc(&self, slug: &str) -> UsecaseResult<Option<String>>;
    /// Пути .md под префиксом (напр. «Projects/uwuwu»), без docs/.
    async fn list_docs(&self, prefix: &str) -> UsecaseResult<Vec<String>>;
    /// Слаги + git blob-sha всех доков (одно дерево): инкрементальный
    /// синк сравнивает sha и читает содержимое ТОЛЬКО изменившихся
    /// (иначе семантик-тик сканирует сотни read_doc до бюджета —
    /// таймаут-каскад 16.11).
    async fn doc_shas(&self) -> UsecaseResult<Vec<(String, String)>>;
    /// Перезапись дока (commit в ветку, ff-only).
    async fn write_doc(&self, slug: &str, content: &str, message: &str) -> UsecaseResult<()>;
    /// Append строки в конец дока (создаёт при отсутствии).
    async fn append_doc(&self, slug: &str, line: &str, message: &str) -> UsecaseResult<()>;
}

/// Коммент задачи в терминах wakeup: идентичность + время создания.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentRef {
    pub id: String,
    pub created_ms: u64,
    pub body: String,
}

/// Ссылка на сессию опенкода исполнителя.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSpec {
    /// cwd сессии на исполнителе (мастер-чекаут или env задачи).
    pub directory: String,
    pub title: String,
    /// Агент сессии: дефолтная или роль конвейера (AgentSlot).
    pub agent: AgentSlot,
    /// Именованный агент опенкода (agents/<name>.md в cwd сессии):
    /// боты с fm agent — методичка в agent-md, не в промптах.
    /// Приоритетнее agent-слота.
    pub custom_agent: Option<String>,
    /// Модель спавна; None — дефолт исполнителя.
    pub model: Option<ModelRef>,
    /// Вариант модели (opencode model.variant, напр. «low»); None — без.
    pub variant: Option<String>,
    /// Bootstrap-сессия подготовки env: НЕ тарифицируется спавн-бюджетом —
    /// логический спавн тарифа = рабочая сессия (e2e 15.11: полный цикл
    /// с ре-энтри съедал 10/10 из-за bootstrap-пар).
    pub bootstrap: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Alive,
    Dead,
    Unknown,
}

/// Последний ответ ассистента сессии (форма message tail сверена живой
/// пробой опенкода 2.3.0: type/finish/content[].text, time.created).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantReply {
    /// ms создания сообщения (базовая метка «до промпта» vs «после»).
    pub created_ms: u64,
    /// Текстовые части ответа (content[].text), склеенные новой строкой.
    pub text: String,
    /// finish-маркер присутствует — ответ завершён (иначе ещё стримится).
    pub finished: bool,
    /// Ответ — самое свежее сообщение сессии: после него нет ни промптов,
    /// ни tool-call'ов. «Ход завершён»: агент ответил и ждёт (#1206).
    pub is_latest: bool,
    /// Сырое значение finish-маркера API («stop» | «tool-calls» | …);
    /// None — сообщение ещё стримится. Различает шаг хода
    /// («tool-calls» — тула позади, ход продолжается) и финал хода
    /// (#1209, живая проба 17.11).
    pub finish: Option<String>,
}

#[async_trait]
pub trait AgentSessionGateway: Send + Sync {
    async fn create_session(&self, spec: &SessionSpec) -> UsecaseResult<String>;
    async fn prompt(&self, session: &str, text: &str) -> UsecaseResult<()>;
    /// Slash-команда сессии (детерминированные shell-блоки — env-подготовка).
    async fn run_command(&self, session: &str, name: &str, args: &str) -> UsecaseResult<()>;
    async fn liveness(&self, session: &str) -> Liveness;
    async fn delete_session(&self, session: &str) -> UsecaseResult<()>;
    /// ms последней активности сессии (последнее сообщение) или None.
    async fn last_activity_ms(&self, session: &str) -> UsecaseResult<Option<u64>>;
    /// Последний ассистентский ответ сессии (для ботов: ожидание реплая).
    async fn last_assistant_reply(&self, session: &str) -> UsecaseResult<Option<AssistantReply>>;
    /// Список сессий исполнителя (для дримера: idle-детект).
    async fn list_sessions(&self) -> UsecaseResult<Vec<SessionInfo>>;
    /// Экспорт сессии (сообщения/сводка) — контент для дримера.
    async fn export_session(&self, session: &str) -> UsecaseResult<String>;
}

/// Сессия исполнителя в терминах дримера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: String,
    pub agent: String,
    /// Директория сессии (cwd); пустая — исполнитель не отдал location:
    /// вне раскладки гарда спавна (#1163), никогда не матчится.
    pub directory: String,
    /// ms эпохи последнего обновления.
    pub updated_ms: u64,
    /// «succeeded»|«failed»|… финал ПОСЛЕДНЕГО завершённого хода (при новом
    /// ходе не очищается — живость не выражает, для этого `busy`).
    pub outcome: String,
    /// Токены сессии (in+out): пустышки-прерывания = 0 — дример их не берёт.
    pub total_tokens: u64,
    /// Ход идёт прямо сейчас (`time.busy > time.idle` в API исполнителя):
    /// семантика LLM-траты для CUP-капа. `outcome` — атрибут последнего
    /// завершённого хода и при новом ходе не очищается, «живость» он не
    /// выражает (разбор #1231).
    pub busy: bool,
}

/// Состояние PR проекта: источник вердиктов автомержа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    /// PR не найден ни по ячейке, ни по ветке.
    Missing,
    /// GitHub считает (mergeable=null / чеки бегут): вердикт следующего тика.
    Pending,
    Open {
        mergeable: bool,
        ci_green: bool,
    },
    Merged,
    Closed,
}

#[async_trait]
pub trait PrGateway: Send + Sync {
    /// Проба PR: по ячейке (URL) либо по head-ветке.
    async fn probe(
        &self,
        project_repo: &str,
        pr_url: Option<&str>,
        branch: Option<&str>,
    ) -> UsecaseResult<PrState>;
    /// Мерж PR (merge-commit).
    async fn merge(&self, project_repo: &str, pr_url: &str) -> UsecaseResult<()>;
}

/// Часы: домен считает минуту UTC; оркестрация — эпоху.
pub trait Clock: Send + Sync {
    fn utc_minute(&self) -> UtcMinute;
    fn now_ms(&self) -> u64;
}

/// Нативные поля issue (сессии/ветка/PR/приоритет/тип): fm-зеркало снесено.
#[async_trait]
pub trait IssueFieldStore: Send + Sync {
    /// Значения полей задачи (непустые).
    async fn load(&self, number: u64) -> UsecaseResult<std::collections::HashMap<String, String>>;
    /// Батч-запись: Some — значение, None — очистка.
    async fn store(&self, number: u64, patch: &[(&str, Option<String>)]) -> UsecaseResult<()>;
}

/// Дайджест задачи для семантического индекса (юзер-стори: «найти решённое
/// раньше по вики/задачам/опыту»).
#[derive(Debug, Clone)]
pub struct IssueDigest {
    pub number: u64,
    pub title: String,
    pub body: String,
}

/// Источник задач для индексации поиска (реализация — hub; memory-стенд — нет).
#[async_trait]
pub trait IssueIndexSource: Send + Sync {
    /// Последние задачи репо-хаба (новейшие первыми), до limit.
    async fn recent_issues(&self, limit: usize) -> UsecaseResult<Vec<IssueDigest>>;
}

/// Персистентное состояние тика (wakeup-дедуп, дример-инвариант).
pub trait TickStateStore: Send + Sync {
    fn last_comment_id(&self, board: &str, number: u64) -> Option<String>;
    /// Последний статус строки, виденный тиком: переход стадии = новый
    /// раунд для свежеглазых слотов (ревью/тест — канон сессий).
    fn last_seen_status(&self, board: &str, number: u64) -> Option<Status>;
    fn set_last_seen_status(&self, board: &str, number: u64, status: Status);
    fn set_last_comment_id(&self, board: &str, number: u64, id: &str);
    /// Дример уже отработал сессию (1 рабочая = 1 дример).
    fn dreamer_done(&self, session: &str) -> bool;
    fn mark_dreamer_done(&self, session: &str);
    /// Дример: спавнов за сутки (day_key = дата JST) — дневной кап.
    fn dreamer_day_spawns(&self, day_key: &str) -> u32;
    fn mark_dreamer_spawn(&self, day_key: &str);
    /// Анти-шторм: ms последнего спавна слота строки (cooldown).
    fn last_spawn_ms(&self, board: &str, number: u64, slot: Slot) -> Option<u64>;
    fn mark_spawn(&self, board: &str, number: u64, slot: Slot, at_ms: u64);
    /// Сталл-детект строки: (статус раунда, ms последнего пинга, счётчик).
    /// Смена статуса строки = новый раунд (счётчик с нуля).
    fn stall_ping(&self, board: &str, number: u64) -> Option<(uwuwu_domain::Status, u64, u32)>;
    fn mark_stall_ping(&self, board: &str, number: u64, status: uwuwu_domain::Status, at_ms: u64);
    /// Сброс раунда сталла на носителе: каунтер, переживший рестарт
    /// процесса, объявляется недействительным — следующий пинг начинает
    /// счёт с единицы (инцидент 18.11 #1281: локального сброса мало,
    /// mark_stall_ping инкрементит stale-каунтер).
    fn clear_stall(&self, board: &str, number: u64);
    /// Новый раунд слота (fresh-eyes): кулдаун прошлого раунда не переносится.
    fn clear_spawn_ms(&self, board: &str, number: u64, slot: Slot);
    /// Аналитика (#1135, замена триажа): кулдауны волн/ревью и цикл-гвард.
    fn last_analys_ms(&self, board: &str, number: u64) -> Option<u64>;
    fn mark_analys_spawn(&self, board: &str, number: u64, at_ms: u64);
    fn last_analys_review_ms(&self, board: &str, number: u64) -> Option<u64>;
    fn mark_analys_review_spawn(&self, board: &str, number: u64, at_ms: u64);
    /// Цикл-гвард аналитики: (статус на прошлом спавне волны, кругов);
    /// строка вне цикла Analys↔Review Analys — счётчик устарел.
    fn analys_loop(&self, board: &str, number: u64) -> Option<(Status, u32)>;
    fn set_analys_loop(&self, board: &str, number: u64, status: Status, rounds: u32);
    fn clear_analys_loop(&self, board: &str, number: u64);
    /// Цикл-гвард ревью-контура (R2-C2): спавны ревьювера при неизменном
    /// статусе строки.
    fn analys_review_loop(&self, board: &str, number: u64) -> Option<(Status, u32)>;
    fn set_analys_review_loop(&self, board: &str, number: u64, status: Status, repeats: u32);
    fn clear_analys_review_loop(&self, board: &str, number: u64);
    /// Гигиена (R2-L3): счётчики обеих гвардов (аналитик + ревьювер) строк,
    /// ушедших с досок, вычищаются — live = ключи «board:number» всех строк
    /// всех досок тика.
    fn prune_analys_guards(&self, live: &std::collections::HashSet<String>);
    /// Attention-механика: счётчик повторных спавнов слота
    /// (статус на момент прошлого спавна, число повторов).
    fn spawn_loop(&self, board: &str, number: u64, slot: Slot) -> Option<(Status, u32)>;
    fn set_spawn_loop(&self, board: &str, number: u64, slot: Slot, status: Status, repeats: u32);
    /// Attention-механика: строка наблюдается в статусе, отличном от
    /// сохранённого у счётчиков её слотов — прошлые попытки спавна
    /// устарели (строка ушла в другую стадию и вернулась), сброс.
    /// Инцидент #1046 (15.11): счётчик (In Progress, 2) пережил путь
    /// IP→Design→Ревью плана→IP и эскалировал первый же спавн.
    fn reset_stale_spawn_loops(&self, board: &str, number: u64, current: Status);
    /// Реестр серверных постов (self-эхо-фильтр #1204): коммент опубликован
    /// агентом через MCP task_comment (не юзером и не фабрикой).
    fn own_comment(&self, id: &str) -> bool;
    fn mark_own_comment(&self, id: &str, posted_ms: u64);
    fn save(&self) -> UsecaseResult<()>;
}
