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
    // имена/парсинг — usecase::naming (конфигурация, не домен)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_variants_canonical() {
        // 11 типов канона — компилятор держит состав
        let _ = [
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
        ];
    }
}
