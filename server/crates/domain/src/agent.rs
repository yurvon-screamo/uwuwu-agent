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
    pub fn new(provider: impl Into<String>, id: impl Into<String>, variant: Option<&str>) -> Self {
        Self {
            model: ModelRef::new(provider, id),
            variant: variant.filter(|v| !v.is_empty()).map(str::to_string),
        }
    }

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
}
