//! TaskStore — контракт task-домена над бекендом. Семантика контракта:
//! read-after-write (мутации возвращают финальное состояние), fail-loud
//! валидации, идемпотентный create (search-before-create; ретрай после
//! таймаута возвращает созданное, дубль — громкий отказ).

use super::{StoreError, Task, TaskBoard, TaskType};
use std::collections::HashMap;

/// Create: тело (md) опционально — шаблон ставит реализация по типу,
/// статус: Draft дефолт; Todo — машинные создания (крон-раны).
#[derive(Clone, Debug)]
pub struct CreateTask {
    pub title: String,
    pub task_type: TaskType,
    pub status: Option<String>,
    pub body_md: Option<String>,
    /// Аттачи: (путь, альт-текст) — медиа в md-референсы тела.
    pub attachments: Vec<(std::path::PathBuf, String)>,
    /// Приоритет (канон CUP); None — не задан (create write-path
    /// подставит дефолт medium — решение владельца 13.11).
    pub priority: Option<String>,
    /// Autodone-галочка (владелец решает; триаж не ставит).
    pub autodone: Option<bool>,
}

/// Edit: все поля опциональны — пишутся только заданные.
#[derive(Clone, Debug, Default)]
pub struct EditTask {
    pub title: Option<String>,
    pub status: Option<String>,
    pub task_type: Option<TaskType>,
    pub branch: Option<String>,
    pub pr: Option<String>,
    pub dev_session: Option<String>,
    /// (дисплей, epoch ms) — паритет клетки «Запланировано».
    pub planned: Option<(String, i64)>,
    /// Приоритет (канон CUP): high/medium/low; "" — снять (дефолта нет).
    pub priority: Option<String>,
    /// Some(true/false) — вкл/выкл autodone, None — не менять.
    pub autodone: Option<bool>,
}

/// Comment insert: семантика append — реализация не дедупит (паритет
/// текущего контура; ретрай может задвоить). id — носимый якорь для reply.
#[derive(Clone, Debug)]
pub struct CommentInsert {
    pub id: String,
}

pub type StoreResult<T> = Result<T, anyhow::Error>;

/// Контракт task-домена. Реализация обязана резолвить `needle` (id, префикс
/// или подстрока тайтла) в скоупе доски проекта: нечёткий — требование
/// уникальности (иначе громкий отказ с раскладкой совпадений).
#[async_trait::async_trait]
pub trait TaskStore: Send + Sync {
    /// Все строки доски проекта (полная выборка, пагинация внутри).
    async fn list(&self, board: &TaskBoard) -> StoreResult<Vec<Task>>;

    /// Резолв needle → задача (id-first, затем тайтл-матчи).
    async fn resolve(&self, board: &TaskBoard, needle: &str) -> Result<Task, StoreError>;

    /// Финальное состояние тела задачи (md).
    async fn body_get(&self, board: &TaskBoard, task: &Task) -> StoreResult<String>;

    /// Полная перезапись тела. `base` — снапшот свежести (тело из get):
    /// реализация отказывает при расхождении с текущим телом —
    /// параллельная правка не затирается.
    async fn body_replace(
        &self,
        board: &TaskBoard,
        task: &Task,
        new_md: &str,
        base: &str,
    ) -> StoreResult<Task>;

    async fn create(&self, board: &TaskBoard, input: &CreateTask) -> Result<Task, StoreError>;

    async fn edit(&self, board: &TaskBoard, task: &Task, patch: &EditTask) -> StoreResult<Task>;

    /// Коммент в задачу; `reply_to` — префикс id верхнеуровневого коммента
    /// (легаси-бекенд: вложенный пункт; GitHub: тред-reply).
    /// Семантика append — реализация не дедупит (ретрай может задвоить,
    /// паритет текущего контура).
    async fn comment(
        &self,
        board: &TaskBoard,
        task: &Task,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<CommentInsert, StoreError>;

    /// Вид задачи как диалога: комменты с якорями (reply-вложенность —
    /// элементы с `reply_to`).
    async fn thread(&self, board: &TaskBoard, task: &Task) -> StoreResult<Vec<ThreadEntry>>;

    /// Ячейка «Сессия dev»: claim агента / запись фабрики.
    async fn claim(&self, board: &TaskBoard, task: &Task, session: &str) -> StoreResult<Task>;

    /// Done: только владелец; note — закрывающий коммент (опц.).
    async fn close(&self, board: &TaskBoard, task: &Task, note: Option<&str>) -> StoreResult<Task>;

    /// Крон-файр: создать ран-карточку «⏰ <мастер> · <дата время>» в скоупе
    /// доски (Todo), вернуть её. Реализация ставит planned = фактический файр.
    /// Default: этап коммутации (ран-путь cmd-слоя переезжает в store) —
    /// громкий отказ.
    async fn create_cron_run(
        &self,
        board: &TaskBoard,
        master: &Task,
        fired_at_ms: i64,
    ) -> Result<Task, StoreError> {
        let _ = (board, master, fired_at_ms);
        Err(StoreError::NotFound {
            needle: "store: create_cron_run не реализован — этап коммутации".into(),
        })
    }

    /// Живой инвентарь досок проектов (list без --project).
    async fn discover_boards(&self) -> StoreResult<Vec<TaskBoard>>;

    /// Машиночитаемые атрибуты тела (fm-шапка: schedule мастера и пр.).
    async fn body_attrs(
        &self,
        board: &TaskBoard,
        task: &Task,
    ) -> StoreResult<HashMap<String, String>>;

    /// Записать атрибут тела (schedule: строка расписания).
    async fn set_body_attr(
        &self,
        board: &TaskBoard,
        task: &Task,
        key: &str,
        value: &str,
    ) -> StoreResult<()>;
}

/// Элемент треда задачи: коммент или reply.
#[derive(Clone, Debug)]
pub struct ThreadEntry {
    pub id: String,
    /// None — верхнеуровневый; Some(id-префикс) — вложенный ответ.
    pub reply_to: Option<String>,
    pub author: String,
    pub text: String,
}
