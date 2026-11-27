//! Нативные ошибки юзкейс-слоя: типизированные контексты отказов
//! портов/оркестрации. Никаких багов-сумок (anyhow) — конкретный вид
//! отказа виден в типе; динамическая деталь живёт в сообщении.

use std::fmt;

/// Контексты отказов по источнику; деталь — в сообщении.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsecaseError {
    /// Доска задач (чтение строк, мутации носителя).
    Board(String),
    /// Исполнитель сессий (опенкод).
    Session(String),
    /// PR-шлюз (пробы/мерж).
    Pr(String),
    /// Персистентное состояние тика.
    State(String),
    /// Telegram Bot API (боты, #1150).
    Telegram(String),
    /// Файл превышает потолок скачивания Bot API (20 MiB) — перманентный
    /// отказ `getFile`, ретрай бессмысленен (#1171).
    TelegramTooBig(String),
    /// Исполнитель отказал в доставке: сессии нет или она недоставляема
    /// (dead-session класс, #1288) — финальная, не временная: ретрай той
    /// же сессии бессмысленен, маршрут — resurrection/парковка.
    SessionNotFound(String),
    /// Blob-канал (#1292): отказ S3-носителя (сеть/статус) или ввода.
    Blob(String),
    /// Файл превышает потолок blob-канала (20 MiB, прецедент UC-06).
    BlobTooBig(String),
    /// Blob-канал не сконфигурирован: не заданы env `UWUWU_S3_*` (#1292).
    BlobNotConfigured(String),
    /// Дедуп-ключ blob не прошёл доменный гард `BlobKey` (#1292).
    BlobBadKey(String),
    /// Append в несуществующий док (#1613): запрет создания в писателе —
    /// append дописывает живой носитель, создание — только docs_write.
    /// Финальный, не временный: ретрай той же строкой бессмысленен.
    AppendMissing(String),
    /// Удаление дока: файла нет в живом чтении — бэкап и мутация не делаются.
    DocAbsent(String),
    /// Удаление дока: источник чтения не доказал содержимое и blob SHA.
    DocReadFailed(String),
    /// Удаление дока: бэкап не состоялся — мутация запрещена.
    BackupFailed(String),
    /// Удаление дока: ref изменился между чтением и коммитом (#1653).
    RaceRef(String),
    /// Удаление дока (#1697): файла уже нет в дереве коммита, с которым
    /// строилось удаление, — конкурент удалил его. Повтор не требуется.
    DocAbsentAtRef(String),
    /// Удаление дока (#1697): дерево коммита недоступно для проверки
    /// версии (статус, усечение, битый ответ, транспорт). Какой-либо
    /// мутации этим вердиктом не обещаем: ответ мог потеряться после PATCH.
    PrecheckUnavailable(String),
    /// Эмбеддинг-маршрут упёрся в лимиты провайдера (HTTP 429, #1363):
    /// временная — ретрай осмыслен в том же часу; бэкофф из RetryInfo.
    RateLimited {
        service: String,
        retry_after_secs: Option<u64>,
    },
}

impl fmt::Display for UsecaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Board(m) => write!(f, "board: {m}"),
            Self::Session(m) => write!(f, "session: {m}"),
            Self::Pr(m) => write!(f, "pr: {m}"),
            Self::State(m) => write!(f, "state: {m}"),
            Self::Telegram(m) => write!(f, "telegram: {m}"),
            Self::TelegramTooBig(m) => write!(f, "telegram too big: {m}"),
            Self::SessionNotFound(m) => write!(f, "session not found (dead-session): {m}"),
            Self::Blob(m) => write!(f, "blob: {m}"),
            Self::BlobTooBig(m) => write!(f, "blob too big: {m}"),
            Self::BlobNotConfigured(m) => write!(f, "blob is not configured: {m}"),
            Self::BlobBadKey(m) => write!(f, "blob bad key: {m}"),
            Self::AppendMissing(m) => write!(f, "append target missing: {m}"),
            Self::DocAbsent(m) => write!(f, "doc absent: {m}"),
            Self::DocReadFailed(m) => write!(f, "doc read failed: {m}"),
            Self::BackupFailed(m) => write!(f, "backup failed: {m}"),
            Self::RaceRef(m) => write!(f, "competing ref: {m}"),
            Self::DocAbsentAtRef(m) => write!(f, "doc absent at ref: {m}"),
            Self::PrecheckUnavailable(m) => write!(f, "precheck unavailable: {m}"),
            Self::RateLimited {
                service,
                retry_after_secs,
            } => match retry_after_secs {
                Some(s) => write!(
                    f,
                    "{service}: rate limited, retry after {s}s ({})",
                    humanize_retry_secs(*s)
                ),
                None => write!(f, "{service}: rate limited"),
            },
        }
    }
}

/// Человекочитаемая длительность retry-after (#1473, срез В): сырые секунды
/// остаются машинным контрактом, добавка — чтобы агент видел масштаб отказа
/// («суточный, не мгновенный») без календарной математики и clock-проводки.
fn humanize_retry_secs(secs: u64) -> String {
    let mins = secs / 60;
    if mins < 60 {
        format!("≈{mins} мин")
    } else {
        let hours = mins / 60;
        let rest = mins % 60;
        if rest == 0 {
            format!("≈{hours} ч")
        } else {
            format!("≈{hours} ч {rest} мин")
        }
    }
}

impl std::error::Error for UsecaseError {}

pub type UsecaseResult<T> = Result<T, UsecaseError>;

/// Отказ чтения (MCP-вьюхи): нет такой доски/дока — честное «не найдено»,
/// транспортные отказы — контекст источника. Нейтральный дом: тип общий
/// для вьюх задач и целей, не собственность неймспейса tasks (#1216).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewError {
    NotFound(String),
    Source(UsecaseError),
}

impl From<UsecaseError> for ViewError {
    fn from(e: UsecaseError) -> Self {
        Self::Source(e)
    }
}

impl std::fmt::Display for ViewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(what) => write!(f, "not found: {what}"),
            Self::Source(e) => write!(f, "{e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limited_display_carries_human_duration() {
        let err = UsecaseError::RateLimited {
            service: "gemini-embeddings".into(),
            retry_after_secs: Some(73370),
        };
        let text = err.to_string();
        assert!(
            text.contains("73370s") && text.contains("≈20 ч 22 мин"),
            "сырые секунды + человекочитаемая длительность, факт: {text}"
        );
    }

    #[test]
    fn rate_limited_display_formats_durations() {
        let fmt = |secs| {
            UsecaseError::RateLimited {
                service: "s".into(),
                retry_after_secs: Some(secs),
            }
            .to_string()
        };
        assert!(fmt(30508).contains("≈8 ч 28 мин"), "{}", fmt(30508));
        assert!(fmt(7200).contains("≈2 ч"), "{}", fmt(7200));
        assert!(fmt(2700).contains("≈45 мин"), "{}", fmt(2700));
    }

    #[test]
    fn rate_limited_without_retry_info_stays_unadorned() {
        let err = UsecaseError::RateLimited {
            service: "gemini-embeddings".into(),
            retry_after_secs: None,
        };
        assert_eq!(err.to_string(), "gemini-embeddings: rate limited");
    }
}
