//! Боты на док-носителе (#1150): value-типы док-спеки и дифа жизненного
//! цикла. Парс fm и диф-логика — юзкейс-слой (uwuwu_usecase::bots).

/// Модель сессии бота: fm `model` = `provider/id[#variant]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotModel {
    pub provider: String,
    pub id: String,
    pub variant: Option<String>,
}

/// Спека бота из `Projects/<proj>/Боты/<имя>.md` (fm-шапка + тело).
/// Тело дока — методичка агента (системный контекст сессии).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotSpec {
    /// Слэг дока (Projects/<proj>/Боты/<имя>) — ключ состояния/раннтайма.
    pub slug: String,
    /// fm title (дефолт — имя дока).
    pub title: String,
    /// fm enabled (дефолт true; false = бот погашен).
    pub enabled: bool,
    /// fm model (обязателен) — сессия биндит модель при создании.
    pub model: BotModel,
    /// fm token_env (обязателен) — имя env-переменной с токеном ТГ.
    pub token_env: String,
    /// fm owner (опц.) — username ТГ-владельца, фильтр «чужим не отвечаем».
    pub owner: Option<String>,
    /// fm workspace (обязателен) — cwd opencode-сессии на исполнителе.
    pub workspace: String,
    /// fm greeting (опц.) — детерминированный ответ на /start без LLM.
    pub greeting: Option<String>,
    /// fm agent (опц.) — имя агента опенкода: методичка живёт в
    /// agents/<name>.md воркспейса и вшита в сессию системой, промпты
    /// несут только текст пользователя (владелец 16.11: «в агента мд
    /// вшить и всё»). Без поля — методичка дока, бутстрап одним
    /// промптом при создании/правке (не префиксом каждого сообщения).
    pub agent: Option<String>,
    /// Тело дока — методичка; в agent-режиме игнорируется.
    pub manual: String,
}

impl BotSpec {
    /// Структурные поля (model, workspace) — смена = пересоздание сессии.
    pub fn structural_changed(&self, other: &BotSpec) -> bool {
        self.model != other.model || self.workspace != other.workspace || self.agent != other.agent
    }

    /// Runtime-поле token_env — смена = stop/start задачи, сессия живёт.
    pub fn token_changed(&self, other: &BotSpec) -> bool {
        self.token_env != other.token_env
    }
}

/// Действие супервизора по дифу желаемого/запущенного (Spawn — ветка
/// «нет записи» супервизора, не значение дифа).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotAction {
    /// enabled:false / снос / переименование дока — погасить задачу
    /// (сессия в opencode.db сохраняется).
    Stop,
    /// Смена model/workspace — задача на новой сессии, память сброшена.
    RecreateSession,
    /// Смена token_env — stop/start задачи, сессия живёт.
    ReArm,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(model: &str, workspace: &str, token_env: &str) -> BotSpec {
        let (provider, rest) = model.split_once('/').unwrap();
        let (id, variant) = rest
            .split_once('#')
            .map_or((rest, None), |(i, v)| (i, Some(v.to_string())));
        BotSpec {
            agent: None,
            slug: "Projects/uwuwu/Боты/x".into(),
            title: "x".into(),
            enabled: true,
            model: BotModel {
                provider: provider.into(),
                id: id.to_string(),
                variant,
            },
            token_env: token_env.into(),
            owner: None,
            workspace: workspace.into(),
            greeting: None,
            manual: "методичка".into(),
        }
    }

    #[test]
    fn structural_tiers() {
        let base = spec("p/a", "/w", "T");
        assert!(!base.structural_changed(&base));
        assert!(base.structural_changed(&spec("p/b", "/w", "T")));
        assert!(base.structural_changed(&spec("p/a", "/w2", "T")));
        assert!(
            !base.structural_changed(&spec("p/a", "/w", "T2")),
            "token — не структурное"
        );
        assert!(base.token_changed(&spec("p/a", "/w", "T2")));
        assert!(!base.token_changed(&spec("p/b", "/w2", "T")));
    }
}
