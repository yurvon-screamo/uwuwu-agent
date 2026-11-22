//! Порты юзкейс-слоя: требования к инфре. Сигнатуры — на доменных типах
//! и нативных ошибках (UsecaseError); инфра (uwuwu-server) реализует их
//! поверх gh/опенкода/ФС.

use crate::error::UsecaseResult;
use async_trait::async_trait;
use uwuwu_domain::{
    Agent, AgentSlot, BranchName, ModelRef, Slot, Status, Task, TaskFields, UtcMinute,
};

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
    /// Предикат присутствия строки на доске — независим от читаемости
    /// Status (строка без поля/в read-лаге всё ещё строка; close-поиск
    /// доски #1236).
    async fn row_exists(&self, board: &str, number: u64) -> UsecaseResult<bool>;
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
    /// идут через IssueFieldStore напрямую. direct (#1357) — как
    /// automerge: состояние на носителе явно, write-through тот же.
    async fn set_board_cells(
        &self,
        board: &str,
        number: u64,
        priority: Option<&str>,
        automerge: Option<bool>,
        direct: Option<bool>,
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
    /// kind_name — имя типа в нативном словаре issue Type (task_type_
    /// native_name; #1271): идёт в теле POST одним вызовом, эхо ответа
    /// сверяется с запросом — «silently dropped» без push-доступа ловится
    /// громко на границе носителя. None — тип не ставится (не бывает из
    /// create-юзкейса: дефолт Feature резолвится выше).
    async fn create_issue(
        &self,
        board: &str,
        project: &str,
        title: &str,
        body: &str,
        kind_name: Option<&str>,
    ) -> UsecaseResult<u64>;
    /// Запись нативного типа issue (PATCH issue {"type": имя}; #1271):
    /// неизвестное имя — HTTP 422 от GitHub (громко через gherr),
    /// тихий дроп без push-доступа ловится сверкой эха ответа.
    async fn set_issue_type(&self, number: u64, kind_name: &str) -> UsecaseResult<()>;
    /// Переименование (тайтл уникален — дубль отвергается юзкейсом выше).
    async fn rename_issue(&self, number: u64, title: &str) -> UsecaseResult<()>;
    /// Точечная правка fm-ячеек (branch/pr/priority/…) с сохранением контента.
    async fn patch_cells(&self, number: u64, mutate: FmMutator) -> UsecaseResult<()>;
    /// Закрытие с причиной (completed|not_planned).
    async fn close_issue_ext(&self, number: u64, not_planned: bool) -> UsecaseResult<()>;
    /// Архивировать строку доски (терминальную — клин холодного ящика
    /// #1293): Projects v2 убирает её из items (снапшот сжимается),
    /// строка остаётся доступной через unarchive. Реализация НЕ обязана
    /// инвалидинировать снапшот на каждую строку — волна архиваций
    /// держит кеш живым (построчный ре-фетч полного снимка выжег бы
    /// бюджет первой волной); свежий срез даёт invalidate_snapshot.
    async fn archive_row(&self, board: &str, number: u64) -> UsecaseResult<()>;
    /// Сброс снапшота доски: реализация с кешем забывает срез —
    /// следующий read_board читает носитель. Клин зовёт один раз на
    /// волну (финал), не на каждую строку. Дефолт — no-op (носители
    /// без кеша).
    fn invalidate_snapshot(&self, _board: &str) {}
    /// Остаток GraphQL-бюджета носителя (точек): None — носитель не
    /// сообщает, бюджет-гайд клина пропускает.
    fn gql_budget_remaining(&self) -> Option<u64> {
        None
    }
    /// Номера issues репо с точно этим тайтлом (включая закрытые):
    /// dup_check create против холодного ящика (#1293) — тайтл уникален
    /// на доске и после архивации старых строк.
    async fn issues_with_title(&self, title: &str) -> UsecaseResult<Vec<u64>>;
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

/// Итог append-мутации дока (#1353): честность ответа read-after-write.
/// `created` — файла не было, док создан этой строкой (иначе — append к
/// существующему телу); `lines` — строк в доке после записи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendDocOutcome {
    pub created: bool,
    pub lines: usize,
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
    /// Append строки в конец дока (создаёт при отсутствии); возвращает
    /// факт операции — создан или append к существующему, строк после
    /// записи (#1353: ответ тула отражает факт, не эхо ввода).
    async fn append_doc(
        &self,
        slug: &str,
        line: &str,
        message: &str,
    ) -> UsecaseResult<AppendDocOutcome>;
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
    /// Модель спавна — обязательна (#1154, решение владельца 17.11:
    /// единый механизм = карта моделей сервера в теле спавна; body.model
    /// перекрывает любые деф-модели, деф-файлы агентов моделей не несут).
    pub model: ModelRef,
    /// Вариант модели (opencode model.variant, напр. «high»); None — без.
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
    /// Пин агента слота на живой сессии перед доставкой хода (#1300):
    /// opencode `POST /session/:id/agent` (switchAgent). Живая проба
    /// 2.3.2: session.agent персистится; prompt-эндпоинт агент-поле не
    /// несёт (openapi additionalProperties:false — тихий discard), поэтому
    /// пин живёт отдельным зовном. Дефолт no-op — гейтвеи без ручки
    /// (фейки портов) не переопределяют; прод-клиент и бюджет-обёртка
    /// несут настоящую механику (прецедент abort_session).
    async fn switch_agent(&self, _session: &str, _agent: Agent) -> UsecaseResult<()> {
        Ok(())
    }
    /// Slash-команда сессии (детерминированные shell-блоки — env-подготовка).
    async fn run_command(&self, session: &str, name: &str, args: &str) -> UsecaseResult<()>;
    async fn liveness(&self, session: &str) -> Liveness;
    async fn delete_session(&self, session: &str) -> UsecaseResult<()>;
    /// Прервать активный ход сессии (преемпшен CUP #1259): opencode
    /// `POST /session/:id/abort` — «stop any ongoing AI processing».
    /// Дефолт Ok(()) — фейки портов не переопределяют; прод-гейтвеи
    /// (opencode, budget-обёртка) несут настоящую ручку.
    async fn abort_session(&self, _session: &str) -> UsecaseResult<()> {
        Ok(())
    }
    /// Бюджетная проба без мутаций (преемпшен #1259: готовность
    /// кандидата проверяется ДО гашения жертвы — жертва зря не гибнет).
    /// Дефолт true — ограничение только у budget-обёртки.
    fn spawn_allowed(&self) -> bool {
        true
    }
    /// Занятость окна контекста сессии, % лимита модели исполнителя
    /// (#1221, ADR-082): токены свежайшего assistant-поворота против
    /// limit.context каталога моделей — `session.tokens` кумулятивен
    /// и непригоден. Ok(None) — не измерить (assistant-поворотов нет /
    /// модели нет в каталоге); Err — транспортный отказ (тик переживает
    /// пропуском). Дефолт None — гейтвеи без ручки (фейки) не измеряют.
    async fn context_usage_pct(
        &self,
        _session: &str,
        _model: &ModelRef,
    ) -> UsecaseResult<Option<f64>> {
        Ok(None)
    }
    /// Нативный компакт сессии (#1221, ADR-082): POST /session/:id/compact —
    /// durable admission («исполнится на ближайшей границе шага», сам будит
    /// idle-луп сессии). Дефолт Ok(()) — фейки портов не переопределяют;
    /// прод-гейтвеи (opencode, budget-обёртка) несут настоящую ручку
    /// (прецедент abort_session).
    async fn compact_session(&self, _session: &str) -> UsecaseResult<()> {
        Ok(())
    }
    /// ms последней активности сессии (последнее сообщение) или None.
    async fn last_activity_ms(&self, session: &str) -> UsecaseResult<Option<u64>>;
    /// Последний ассистентский ответ сессии (для ботов: ожидание реплая).
    async fn last_assistant_reply(&self, session: &str) -> UsecaseResult<Option<AssistantReply>>;
    /// Дождаться user-сообщения сессии, содержащего `marker`, не дольше
    /// `deadline_ms`; вердикты env-gc (#1431): shell-вывод slash-команды
    /// доезжает user-сообщением bootstrap-сессии. None — дедлайн. Опрос
    /// с паузами — инфра-забота адаптера (runtime), юзкейс агностичен.
    /// Дефолт None: фейки без env-gc не должны уметь канал.
    async fn wait_user_message(
        &self,
        _session: &str,
        _marker: &str,
        _deadline_ms: u64,
    ) -> UsecaseResult<Option<String>> {
        Ok(None)
    }
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
    /// PR-URL задан в PR-форме, но вендор хоста гейтвеем не управляется
    /// (чужой GHE-хост, Gitea-инстанс без токена, API-отказ инстанса):
    /// строка ждёт владельца в Review — не bounce (#1269).
    Foreign,
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

/// Трек незавершённого крон-рана (#1480, снос журнала): файр записан —
/// ран ждёт факта завершения от сессии исполнителя. Пустая строка сессии —
/// краш между записью файра и спавном: тишина-детект ведёт по fired_ms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronRunTrack {
    pub session: String,
    pub fired_ms: u64,
}

/// Машинный учёт крона (state.json, секция `crons`, #1480 — снос журнала):
/// last_fire — факт файра (источник is_due вместо журнальной строки),
/// run — незавершённый ран (детект тишины), deferred_since — файр due,
/// но отложен (детект пропуска), fails — счётчик отказов (канон ADR-103
/// «счётчики отказов — в state»).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CronRow {
    pub last_fire_ms: Option<u64>,
    pub deferred_since_ms: Option<u64>,
    pub run: Option<CronRunTrack>,
    pub fails: u32,
}

/// Персистентное состояние кронов — третий делегат JsonStateStore
/// (рядом с TickStateStore/BotStateStore); md-мастер остаётся чистой
/// декларацией, runtime-факты — в state (канон ADR-103).
pub trait CronStateStore: Send + Sync {
    fn cron_row(&self, name: &str) -> CronRow;
    fn set_cron_row(&self, name: &str, row: CronRow);
    fn save(&self) -> UsecaseResult<()>;
}

/// Часы: домен считает минуту UTC; оркестрация — эпоху.
pub trait Clock: Send + Sync {
    fn utc_minute(&self) -> UtcMinute;
    fn now_ms(&self) -> u64;
    /// Пауза между контрольными чтениями носителя после мутации (#1331,
    /// план-контракт «3×1с»): смягчает ложные Stale-отказы на секундном
    /// лаге видимости носителя. Дефолт — мгновенный возврат: фейки часов
    /// тестов не спят; SystemClock спит реально.
    fn sleep_ms(&self, _ms: u64) {}
}

/// Конфиг судьи `[judge]` (#1364): значения — из конфига сервера, дефолты —
/// канон плана. `enabled` — opt-in (дефолт off): контур включается решением
/// владельца на деплое.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeCfg {
    pub enabled: bool,
    /// Интервал повторных вызовов судьи на одной тишине (ms).
    pub every_ms: u64,
    /// Гвард сверху судьи: вызовов без смены статуса строки до
    /// принудительного паркинга (слово владельца «порог 3»).
    pub escalate_after: u32,
    /// Хвост ленты: комментов в контексте.
    pub comments_tail: usize,
    /// Трим одного коммента (символы, char-safe).
    pub comment_max_chars: usize,
    /// Трим тела задачи (символы) — тела регулярно 10–40k (ревью C3).
    pub body_max_chars: usize,
    /// Трим одного ассистентского ответа в хвосте сессии (символы).
    pub msg_max_chars: usize,
}

impl Default for JudgeCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            every_ms: 2 * 60 * 60_000,
            escalate_after: 3,
            comments_tail: 4,
            comment_max_chars: 1500,
            body_max_chars: 6000,
            msg_max_chars: 800,
        }
    }
}

/// Хвост сессии слота в контексте судьи: последний ассистентский ответ
/// (трим `msg_max_chars`) + признак завершённого хода.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeSessionTail {
    pub slot: Slot,
    pub text: String,
    pub finished: bool,
}

/// Контекст судьи (#1364): всё, что тик знает о строке. Тело и лента —
/// носитель задачи (не вердикты агентов); хвосты сессий — состояние
/// исполнителя. Тримы — квотная дисциплина (ревью C3: тела 10–40k).
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeContext {
    pub board: String,
    pub number: u64,
    pub title: String,
    pub status: Status,
    pub slot: Slot,
    /// Тишина stage-сессии (мин) и признак завершённого хода.
    pub silence_min: u64,
    pub idle: bool,
    /// Вызовов судьи без смены статуса строки (гвард сверху).
    pub judge_calls_no_progress: u32,
    /// Подряд request_changes ревью-контура (кап-контекст).
    pub review_rounds: u32,
    pub branch: Option<String>,
    pub pr_url: Option<String>,
    /// Сервер перезапущен N мин назад: структурный сигнал масс-аномалий
    /// сталл-класса #1281 (каунтеры из-под рестарта).
    pub restart_recent_min: Option<u64>,
    /// Тело задачи (трим `body_max_chars`).
    pub body: String,
    /// Хвост ленты (от новых к старым): (создан ms, тело тримнутое).
    pub comments: Vec<(u64, String)>,
    /// Хвосты сессий слотов с назначениями.
    pub sessions: Vec<JudgeSessionTail>,
    /// Эпоха сборки контекста: возраст ленты в промпте (ревью L2).
    pub now_ms: u64,
    /// Точка капа ревью-раундов: тишина/idle не меряются (сталл-гейт не
    /// звучал) — промпт не сообщает ложное «тишина 0 мин, ход завершён»
    /// (ревью раунда 2, L).
    pub cap_point: bool,
}

/// Судья — decision-модель статусных решений (#1364). Err — отказ вызова
/// (сбой сети / 429 / локальный лимит полосы): тик падает в старую
/// механику — судья улучшение, не зависимость.
#[async_trait]
pub trait DecisionJudge: Send + Sync {
    async fn decide(&self, ctx: &JudgeContext) -> UsecaseResult<uwuwu_domain::JudgeVerdict>;
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

/// Метка дример-обработки сессии (#1306, инвариант 1:1 — ADR-005):
/// watermark вместо вечного маркера — revival (активность выше позиции)
/// снова делает сессию кандидатом.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DreamerMark {
    /// Прогон дримера покрывал активность сессии до этой метки
    /// (ms последнего обновления источника на момент прогона).
    UpTo(u64),
    /// Не источник опыта (боты): никогда не дримить.
    Forever,
}

/// Персистентное состояние тика (wakeup-дедуп, дример-инвариант).
pub trait TickStateStore: Send + Sync {
    fn last_comment_id(&self, board: &str, number: u64) -> Option<String>;
    /// Последний статус строки, виденный тиком: переход стадии = новый
    /// раунд для свежеглазых слотов (ревью/тест — канон сессий).
    fn last_seen_status(&self, board: &str, number: u64) -> Option<Status>;
    fn set_last_seen_status(&self, board: &str, number: u64, status: Status);
    fn set_last_comment_id(&self, board: &str, number: u64, id: &str);
    /// Метка дример-обработки: None — сессия ещё не дримлена.
    fn dreamer_mark(&self, session: &str) -> Option<DreamerMark>;
    fn mark_dreamer(&self, session: &str, mark: DreamerMark);
    /// Связка «исходная → её дример-сессия» (инвариант 1:1 — ADR-005).
    fn dreamer_session(&self, source: &str) -> Option<String>;
    fn link_dreamer_session(&self, source: &str, dreamer: &str);
    /// Гигиена: метки и линки сессий, которых нет в живом union-скане,
    /// сносятся (рост state ограничен живыми сессиями, #1306).
    fn prune_dreamer(&self, live: &std::collections::HashSet<String>);
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
    /// Потерянные доставки строки (#1288): (статус раунда, счётчик подряд).
    /// Смена статуса строки = новый раунд; успех доставки сбрасывает.
    fn lost_session(&self, board: &str, number: u64) -> Option<(Status, u32)>;
    fn mark_lost_session(&self, board: &str, number: u64, status: Status);
    fn clear_lost_session(&self, board: &str, number: u64);
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
    /// Гашение преемпшеном (#1412): счётчик слота жертвы сбрасывается —
    /// возобновление после отобранного слота не «спавн без прогресса».
    fn clear_spawn_loop(&self, board: &str, number: u64, slot: Slot);
    /// Attention-механика: строка наблюдается в статусе, отличном от
    /// сохранённого у счётчиков её слотов — прошлые попытки спавна
    /// устарели (строка ушла в другую стадию и вернулась), сброс.
    /// Инцидент #1046 (15.11): счётчик (In Progress, 2) пережил путь
    /// IP→Design→Ревью плана→IP и эскалировал первый же спавн.
    fn reset_stale_spawn_loops(&self, board: &str, number: u64, current: Status);
    /// Цикл-гвард ревью-контура (#1311): счёт подряд request_changes
    /// (переходы ReviewResult→InProgress / ReviewPlan→Design) без
    /// approve. Персистентный, смены статуса внутри цикла не сбрасывают —
    /// в отличие от spawn_loop (слепота attention-гварда к циклу ревью).
    fn review_rounds(&self, board: &str, number: u64) -> u32;
    fn set_review_rounds(&self, board: &str, number: u64, rounds: u32);
    /// Реестр серверных постов (self-эхо-фильтр #1204): коммент опубликован
    /// агентом через MCP task_comment (не юзером и не фабрикой).
    fn own_comment(&self, id: &str) -> bool;
    fn mark_own_comment(&self, id: &str, posted_ms: u64);
    fn save(&self) -> UsecaseResult<()>;
}
