//! Naming: маппинг доменных значений ↔ канонические имена носителей
//! (доска/опенкод/поля issue/state.json). Имена — конфигурация платформы,
//! не бизнес-логика: домен не знает, как он называется снаружи. Меняется
//! носитель — меняется этот модуль, домен нет.

use uwuwu_domain::{Agent, Priority, Schedule, ScheduleKind, Slot, Status, TaskType};

/// Каноническое имя статуса (колонка доски).
pub fn status_name(s: Status) -> &'static str {
    match s {
        Status::Draft => "Draft",
        Status::Todo => "Todo",
        Status::Design => "Design",
        Status::ReviewPlan => "Ревью плана",
        Status::InProgress => "In Progress",
        Status::ReviewResult => "Ревью результата",
        Status::Testing => "Тестирование",
        Status::UserReview => "Review",
        Status::Done => "Done",
        Status::NeedsAttention => "Needs attention",
        Status::Wait => "Вейт",
        Status::Deleted => "Deleted",
        Status::Empty => "",
    }
}

/// Парсинг статуса с доски. Неизвестное имя → Empty: строка выпадает из
/// рабочих списков (легаси/мусор); логирование сырых имён — дело инфры.
pub fn parse_status(raw: &str) -> Status {
    match raw.trim() {
        "Draft" => Status::Draft,
        "Todo" => Status::Todo,
        "Design" => Status::Design,
        "Ревью плана" => Status::ReviewPlan,
        "In Progress" => Status::InProgress,
        "Ревью результата" => Status::ReviewResult,
        "Тестирование" => Status::Testing,
        "Review" => Status::UserReview,
        "Done" => Status::Done,
        "Needs attention" => Status::NeedsAttention,
        "Вейт" => Status::Wait,
        "Deleted" => Status::Deleted,
        _ => Status::Empty,
    }
}

/// Каноническое имя агента опенкода (agent-параметр create_session).
pub fn agent_name(a: Agent) -> &'static str {
    match a {
        Agent::Developer => "developer",
        Agent::PlanReviewer => "plan-reviewer",
        Agent::CodeQualityReviewer => "code-quality-reviewer",
        Agent::Tester => "tester",
        Agent::Dreamer => "dreamer",
        Agent::Triage => "triage",
    }
}

/// Парсинг имени агента (сессии исполнителя, конфиг крона).
pub fn parse_agent(raw: &str) -> Option<Agent> {
    Some(match raw.trim() {
        "developer" => Agent::Developer,
        "plan-reviewer" => Agent::PlanReviewer,
        "code-quality-reviewer" => Agent::CodeQualityReviewer,
        "tester" => Agent::Tester,
        "dreamer" => Agent::Dreamer,
        "triage" => Agent::Triage,
        _ => return None,
    })
}

/// Каноническое имя типа задачи (поле issue).
pub fn task_type_name(t: TaskType) -> &'static str {
    match t {
        TaskType::Bug => "bug",
        TaskType::Spike => "spike",
        TaskType::Analys => "analys",
        TaskType::Docs => "docs",
        TaskType::Other => "other",
        TaskType::Feature => "feature",
        TaskType::Refactoring => "refactoring",
        TaskType::Legacy => "legacy",
        TaskType::Idea => "idea",
        TaskType::Qa => "qa",
        TaskType::Infrastructure => "infrastructure",
    }
}

/// Парсинг типа; None — неизвестное имя.
pub fn parse_task_type(raw: &str) -> Option<TaskType> {
    Some(match raw.trim().to_lowercase().as_str() {
        "bug" => TaskType::Bug,
        "spike" => TaskType::Spike,
        "analys" | "analysis" => TaskType::Analys,
        "docs" => TaskType::Docs,
        "other" => TaskType::Other,
        "feature" => TaskType::Feature,
        "refactoring" => TaskType::Refactoring,
        "legacy" => TaskType::Legacy,
        "idea" => TaskType::Idea,
        "qa" => TaskType::Qa,
        "infrastructure" => TaskType::Infrastructure,
        _ => return None,
    })
}

/// Каноническое имя приоритета (поле issue).
pub fn priority_name(p: Priority) -> &'static str {
    match p {
        Priority::High => "high",
        Priority::Medium => "medium",
        Priority::Low => "low",
        Priority::Empty => "",
    }
}

/// Парсинг приоритета; пустое/неизвестное → Empty (ниже low в очереди CUP).
pub fn parse_priority(raw: &str) -> Priority {
    match raw.trim().to_lowercase().as_str() {
        "high" => Priority::High,
        "medium" => Priority::Medium,
        "low" => Priority::Low,
        _ => Priority::Empty,
    }
}

/// Стабильный ключ слота для персист-состояний (кулдауны/лупы state.json).
pub fn slot_key(slot: Slot) -> &'static str {
    match slot {
        Slot::Design => "design",
        Slot::Dev => "dev",
        Slot::PlanReview => "plan-review",
        Slot::ResultReview => "result-review",
        Slot::Test => "test",
    }
}

/// Человекочитаемый вид расписания (инвентарь cron_list).
pub fn schedule_label(s: &Schedule) -> String {
    match s.kind() {
        ScheduleKind::Interval(m) => format!("каждые {} мин", m.get()),
        ScheduleKind::Daily(at) => format!(
            "ежедневно {:02}:{:02} JST",
            at.minute_of_day() / 60,
            at.minute_of_day() % 60
        ),
        ScheduleKind::Once { at, on } => {
            let (day, month) = on.parts();
            format!(
                "разово {:02}:{:02} {day:02}.{month:02}",
                at.minute_of_day() / 60,
                at.minute_of_day() % 60
            )
        }
        ScheduleKind::Cron(_) => "cron-expr (JST)".into(),
    }
}

/// Все типы канона (порядок = порядок опций поля «Тип»).
pub const TASK_TYPES: [TaskType; 11] = [
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trip() {
        for s in [
            Status::Draft,
            Status::Todo,
            Status::Design,
            Status::ReviewPlan,
            Status::InProgress,
            Status::ReviewResult,
            Status::Testing,
            Status::UserReview,
            Status::Done,
            Status::NeedsAttention,
            Status::Wait,
            Status::Deleted,
        ] {
            assert_eq!(parse_status(status_name(s)), s);
        }
        assert_eq!(parse_status("мусор"), Status::Empty);
        assert_eq!(parse_status(""), Status::Empty);
    }

    #[test]
    fn agent_and_type_round_trip() {
        for a in [
            Agent::Developer,
            Agent::PlanReviewer,
            Agent::CodeQualityReviewer,
            Agent::Tester,
            Agent::Dreamer,
            Agent::Triage,
        ] {
            assert_eq!(parse_agent(agent_name(a)), Some(a));
        }
        assert_eq!(parse_agent("nope"), None);
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
            assert_eq!(parse_task_type(task_type_name(t)), Some(t));
        }
    }

    #[test]
    fn slot_keys_unique() {
        let keys = [
            slot_key(Slot::Design),
            slot_key(Slot::Dev),
            slot_key(Slot::PlanReview),
            slot_key(Slot::ResultReview),
            slot_key(Slot::Test),
        ];
        for i in 0..keys.len() {
            for j in i + 1..keys.len() {
                assert_ne!(keys[i], keys[j]);
            }
        }
    }
}
