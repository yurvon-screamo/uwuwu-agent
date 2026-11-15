//! Агенты конвейера: жёсткая привязка слот стадии → агент (канон фабрики,
//! методички agents/*.md). Дример/триаж — отдельные значения, не слоты стадий.

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
    /// Триаж: валидация черновиков досок.
    Triage,
}

impl Agent {
    /// Имя агента опенкода (agent-параметр create_session).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Developer => "developer",
            Self::PlanReviewer => "plan-reviewer",
            Self::CodeQualityReviewer => "code-quality-reviewer",
            Self::Tester => "tester",
            Self::Dreamer => "dreamer",
            Self::Triage => "triage",
        }
    }

    /// Парсинг имени агента (сессии исполнителя, конфиг крона).
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw.trim() {
            "developer" => Self::Developer,
            "plan-reviewer" => Self::PlanReviewer,
            "code-quality-reviewer" => Self::CodeQualityReviewer,
            "tester" => Self::Tester,
            "dreamer" => Self::Dreamer,
            "triage" => Self::Triage,
            _ => return None,
        })
    }

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
    fn agent_round_trip() {
        for a in [
            Agent::Developer,
            Agent::PlanReviewer,
            Agent::CodeQualityReviewer,
            Agent::Tester,
            Agent::Dreamer,
            Agent::Triage,
        ] {
            assert_eq!(Agent::parse(a.as_str()), Some(a));
        }
        assert_eq!(Agent::parse("nope"), None);
    }

    #[test]
    fn model_ref_accessors() {
        let m = ModelRef::new("zai", "glm-4.7");
        assert_eq!(m.provider(), "zai");
        assert_eq!(m.id(), "glm-4.7");
    }
}
