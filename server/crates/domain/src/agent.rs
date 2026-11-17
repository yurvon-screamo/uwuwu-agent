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
    /// Дример: ночная обработка сессий в опыт.
    Dreamer,
    /// Аналитик: волновая проработка строк Analys (замена триажа, #1135).
    Analyst,
    /// Ревьювер аналитики: парный контур к волнам аналитика.
    AnalystReviewer,
}

impl Agent {
    /// Жёсткая привязка слот стадии → агент (канон конвейера).
    /// Dev/Design — дефолтная сессия исполнителя, без кастомного агента.
    pub fn of_slot(slot: Slot) -> Option<Self> {
        match slot {
            Slot::Design => None,
            Slot::Dev => None,
            Slot::PlanReview => Some(Self::PlanReviewer),
            Slot::ResultReview => Some(Self::CodeQualityReviewer),
            Slot::Test => Some(Self::Tester),
        }
    }

    /// Эмодзи-маркер роли в тайтле спавн-сессии (1:1 черновику владельца,
    /// задача #1212). Пара Design/Dev == 🔨 осознанна и связана тестом
    /// в task.rs — слот без агента это тот же исполнитель.
    pub fn emoji(self) -> &'static str {
        match self {
            Self::Developer => "🔨",
            Self::PlanReviewer => "🔍",
            Self::CodeQualityReviewer => "🔬",
            Self::Tester => "🧪",
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
        assert_eq!(Agent::of_slot(Slot::Dev), None, "dev — дефолтная сессия");
        assert_eq!(Agent::of_slot(Slot::Design), None);
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
