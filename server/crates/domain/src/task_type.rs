//! Тип задачи (колонка «Тип» доски; канон AGENTS.md). Управляет пайплайном:
//! Bug → строго TDD, Spike → код не писать. Idea/Analys выведены из канона
//! (#1135): идеи прорабатывает аналитик до feature-уровня.

/// Тип задачи канона досок uwuwu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskType {
    Bug,
    Spike,
    Docs,
    Other,
    Feature,
    Refactoring,
    Legacy,
    Qa,
    Infrastructure,
}

impl TaskType {
    // имена/парсинг — usecase::naming (конфигурация, не домен)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_variants_canonical() {
        // 9 типов канона (idea/analys удалены #1135) — компилятор держит состав
        let _ = [
            TaskType::Bug,
            TaskType::Spike,
            TaskType::Docs,
            TaskType::Other,
            TaskType::Feature,
            TaskType::Refactoring,
            TaskType::Legacy,
            TaskType::Qa,
            TaskType::Infrastructure,
        ];
    }
}
