//! Тип задачи (колонка «Тип» доски; канон AGENTS.md). Управляет пайплайном:
//! Bug → строго TDD, Spike → код не писать, Analys → находки без фиксов.

/// Тип задачи канона досок uwuwu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskType {
    Bug,
    Spike,
    Analys,
    Docs,
    Other,
    Feature,
    Refactoring,
    Legacy,
    Idea,
    Qa,
    Infrastructure,
}

impl TaskType {
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw.trim().to_lowercase().as_str() {
            "bug" => Self::Bug,
            "spike" => Self::Spike,
            "analys" | "analysis" => Self::Analys,
            "docs" => Self::Docs,
            "other" => Self::Other,
            "feature" => Self::Feature,
            "refactoring" => Self::Refactoring,
            "legacy" => Self::Legacy,
            "idea" => Self::Idea,
            "qa" => Self::Qa,
            "infrastructure" => Self::Infrastructure,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bug => "bug",
            Self::Spike => "spike",
            Self::Analys => "analys",
            Self::Docs => "docs",
            Self::Other => "other",
            Self::Feature => "feature",
            Self::Refactoring => "refactoring",
            Self::Legacy => "legacy",
            Self::Idea => "idea",
            Self::Qa => "qa",
            Self::Infrastructure => "infrastructure",
        }
    }

    /// Дефолт создания (канон: create без типа — feature).
    pub fn default_or_parse(raw: &str) -> Self {
        Self::parse(raw).unwrap_or(Self::Feature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for t in [
            TaskType::Bug,
            TaskType::Spike,
            TaskType::Analys,
            TaskType::Docs,
            TaskType::Other,
            TaskType::Feature,
            TaskType::Refactoring,
            TaskType::Legacy,
            TaskType::Idea,
            TaskType::Qa,
            TaskType::Infrastructure,
        ] {
            assert_eq!(TaskType::parse(t.as_str()), Some(t));
        }
    }

    #[test]
    fn unknown_falls_to_feature() {
        assert_eq!(TaskType::default_or_parse("nope"), TaskType::Feature);
        assert_eq!(TaskType::default_or_parse(""), TaskType::Feature);
        assert_eq!(TaskType::default_or_parse("Qa"), TaskType::Qa, "кейс-инсенсивно");
    }
}
