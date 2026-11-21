//! Агенты конвейера: жёсткая привязка слот стадии → агент (канон фабрики,
//! методички agents/*.md). Дример/аналитик — отдельные значения, не слоты
//! стадий.

use crate::task::Slot;

/// Агент опенкода (primary-агенты фабрики).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Developer,
    PlanReviewer,
    CodeQualityReviewer,
    Tester,
    /// Ревьювер тестового кода (#1413): стадия «Ревью тестирования».
    TestReviewer,
    /// Дример: ночная обработка сессий в опыт.
    Dreamer,
    /// Аналитик: волновая проработка строк Analys (замена триажа, #1135).
    Analyst,
    /// Ревьювер аналитики: парный контур к волнам аналитика.
    AnalystReviewer,
}

impl Agent {
    /// Жёсткая привязка слот стадии → агент (канон конвейера).
    /// #1300: Design/Dev пинятся на явного developer — опора на
    /// ambient-дефолт исполнителя (`default_agent` опенкода) при сбое
    /// резолва подменяла агента дев-сессии (инцидент #1044).
    pub fn of_slot(slot: Slot) -> Option<Self> {
        match slot {
            Slot::Design | Slot::Dev => Some(Self::Developer),
            Slot::PlanReview => Some(Self::PlanReviewer),
            Slot::ResultReview => Some(Self::CodeQualityReviewer),
            Slot::Test => Some(Self::Tester),
            Slot::TestReview => Some(Self::TestReviewer),
        }
    }

    /// Эмодзи-маркер роли в тайтле спавн-сессии (1:1 черновику владельца,
    /// задача #1212). Пара Design/Dev == 🔨 осознанна и связана тестом
    /// в task.rs — оба слота несут одного исполнителя (пин Developer,
    /// #1300).
    pub fn emoji(self) -> &'static str {
        match self {
            Self::Developer => "🔨",
            Self::PlanReviewer => "🔍",
            Self::CodeQualityReviewer => "🔬",
            Self::Tester => "🧪",
            Self::TestReviewer => "🧫",
            Self::Dreamer => "🌙",
            Self::Analyst => "🧠",
            Self::AnalystReviewer => "⚖️",
        }
    }
}

/// Агент исполнения: явное «дефолтная сессия» вместо Option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentSlot {
    #[default]
    Default,
    Role(Agent),
}

impl AgentSlot {
    pub fn of(agent: Option<Agent>) -> Self {
        match agent {
            Some(a) => Self::Role(a),
            None => Self::Default,
        }
    }
}

/// Модель спавна: provider + id (opaque для домена, кроме имён).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    provider: String,
    id: String,
}

impl ModelRef {
    pub fn new(provider: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            id: id.into(),
        }
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Модель спавна с вариантом: `provider/id[#variant]` (канон #1154 —
/// единственный механизм конфигурации моделей: карта сервера в теле
/// спавна, деф-файлы агентов моделей не несут).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnModel {
    pub model: ModelRef,
    pub variant: Option<String>,
}

impl SpawnModel {
    /// `provider/id[#variant]` → SpawnModel.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let (prov_id, variant) = raw
            .split_once('#')
            .map_or((raw, None), |(p, v)| (p, Some(v.trim().to_string())));
        let (provider, id) = prov_id
            .split_once('/')
            .ok_or("модель не в форме provider/id")?;
        if provider.trim().is_empty() || id.trim().is_empty() {
            return Err("модель: пустой provider или id".into());
        }
        Ok(Self {
            model: ModelRef::new(provider.trim(), id.trim()),
            variant: variant.filter(|v| !v.is_empty()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_agent_binding_is_hard() {
        assert_eq!(Agent::of_slot(Slot::PlanReview), Some(Agent::PlanReviewer));
        assert_eq!(
            Agent::of_slot(Slot::ResultReview),
            Some(Agent::CodeQualityReviewer)
        );
        assert_eq!(Agent::of_slot(Slot::Test), Some(Agent::Tester));
        assert_eq!(Agent::of_slot(Slot::TestReview), Some(Agent::TestReviewer));
        // #1300: дев-семья пинится на явного developer — опора на
        // ambient-дефолт исполнителя подменяла агента дев-сессии
        // (инцидент #1044: рестарт будил ход под аналитиком)
        assert_eq!(Agent::of_slot(Slot::Dev), Some(Agent::Developer));
        assert_eq!(Agent::of_slot(Slot::Design), Some(Agent::Developer));
    }

    #[test]
    fn agent_slot_explicit() {
        assert_eq!(AgentSlot::of(None), AgentSlot::Default);
        assert_eq!(
            AgentSlot::of(Some(Agent::Dreamer)),
            AgentSlot::Role(Agent::Dreamer)
        );
    }

    #[test]
    fn model_ref_accessors() {
        let m = ModelRef::new("zai", "glm-4.7");
        assert_eq!(m.provider(), "zai");
        assert_eq!(m.id(), "glm-4.7");
    }

    /// Маркеры ролей попарно различимы (критерий #1212: по списку сессий
    /// мгновенно видно, где какая стадия).
    #[test]
    fn agent_emojis_are_pairwise_distinct() {
        let emojis: [&str; 7] = [
            Agent::Developer.emoji(),
            Agent::PlanReviewer.emoji(),
            Agent::CodeQualityReviewer.emoji(),
            Agent::Tester.emoji(),
            Agent::Dreamer.emoji(),
            Agent::Analyst.emoji(),
            Agent::AnalystReviewer.emoji(),
        ];
        for i in 0..emojis.len() {
            for j in i + 1..emojis.len() {
                assert_ne!(emojis[i], emojis[j]);
            }
        }
    }
}
