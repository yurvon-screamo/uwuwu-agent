//! Naming: маппинг доменных значений ↔ канонические имена носителей
//! (доска/опенкод/поля issue/state.json). Имена — конфигурация платформы,
//! не бизнес-логика: домен не знает, как он называется снаружи. Меняется
//! носитель — меняется этот модуль, домен нет.

use uwuwu_domain::{Agent, Priority, Schedule, ScheduleKind, Slot, Status, TaskType};

/// Каноническое имя статуса (колонка доски).
pub fn status_name(s: Status) -> &'static str {
    match s {
        Status::Draft => "Draft",
        Status::Analys => "Analys",
        Status::ReviewAnalys => "Review Analys",
        Status::Todo => "Todo",
        Status::Design => "Design",
        Status::ReviewPlan => "Ревью плана",
        Status::InProgress => "In Progress",
        Status::ReviewResult => "Ревью результата",
        Status::Testing => "Тестирование",
        Status::ReviewTesting => "Ревью тестирования",
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
        "Analys" => Status::Analys,
        "Review Analys" => Status::ReviewAnalys,
        "Todo" => Status::Todo,
        "Design" => Status::Design,
        "Ревью плана" => Status::ReviewPlan,
        "In Progress" => Status::InProgress,
        "Ревью результата" => Status::ReviewResult,
        "Тестирование" => Status::Testing,
        "Ревью тестирования" => Status::ReviewTesting,
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
        Agent::TestReviewer => "test-reviewer",
        Agent::Dreamer => "dreamer",
        Agent::Analyst => "analyst",
        Agent::AnalystReviewer => "analyst-reviewer",
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
        "analyst" => Agent::Analyst,
        "analyst-reviewer" => Agent::AnalystReviewer,
        _ => return None,
    })
}

/// Каноническое имя типа задачи (поле issue).
pub fn task_type_name(t: TaskType) -> &'static str {
    match t {
        TaskType::Bug => "bug",
        TaskType::Spike => "spike",
        TaskType::Docs => "docs",
        TaskType::Other => "other",
        TaskType::Feature => "feature",
        TaskType::Refactoring => "refactoring",
        TaskType::Legacy => "legacy",
        TaskType::Qa => "qa",
        TaskType::Infrastructure => "infrastructure",
    }
}

/// Парсинг типа; None — неизвестное имя (idea/analys выведены #1135:
/// старые значения читаются как пустой тип, без громких ошибок).
pub fn parse_task_type(raw: &str) -> Option<TaskType> {
    Some(match raw.trim().to_lowercase().as_str() {
        "bug" => TaskType::Bug,
        "spike" => TaskType::Spike,
        "docs" => TaskType::Docs,
        "other" => TaskType::Other,
        "feature" => TaskType::Feature,
        "refactoring" => TaskType::Refactoring,
        "legacy" => TaskType::Legacy,
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

/// Имя нативного поля issue с приоритетом (чтение enrichment и запись
/// create — одно знание; snapshot-колонка доски зовётся тем же словом).
pub const F_PRIORITY: &str = "Приоритет";

/// Единое имя поля автомержа (#1232): записывается create/task_edit.
/// Legacy «Autodone» резолвится на границе носителя (fields.rs store,
/// item_values, parse_field_values — read-нормализация #1280) — сервер
/// читает/пишет оба имени, наружу всегда новое.
pub const F_AUTOMERGE: &str = "Automerge";

/// Единое имя поля direct (#1357): пишется только create (флаг —
/// свойство рождения строки, task_edit его не принимает).
pub const F_DIRECT: &str = "Direct";

/// Парсинг приоритета; пустое/неизвестное → Empty (ниже low в очереди CUP).
/// Один парсер на все носители (snapshot, fm, enrichment): английские и
/// русские варианты опций, регистр и пробелы не влияют.
pub fn parse_priority(raw: &str) -> Priority {
    match raw.trim().to_lowercase().as_str() {
        "high" | "высокий" => Priority::High,
        "medium" | "средний" => Priority::Medium,
        "low" | "низкий" => Priority::Low,
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
        Slot::TestReview => "test-review",
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
pub const TASK_TYPES: [TaskType; 9] = [
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_parsing_unified() {
        // один парсер на все носители: английские опции доски, русские
        // варианты, регистр и пробелы — всё доезжает до домена
        assert_eq!(parse_priority("high"), Priority::High);
        assert_eq!(parse_priority("High "), Priority::High);
        assert_eq!(parse_priority("высокий"), Priority::High);
        assert_eq!(parse_priority("Средний"), Priority::Medium);
        assert_eq!(parse_priority(" низкий "), Priority::Low);
        assert_eq!(parse_priority("LOW"), Priority::Low);
        assert_eq!(parse_priority(""), Priority::Empty);
        assert_eq!(parse_priority("что-то-иное"), Priority::Empty);
    }

    #[test]
    fn status_round_trip() {
        for s in [
            Status::Draft,
            Status::Analys,
            Status::ReviewAnalys,
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
            Agent::Analyst,
            Agent::AnalystReviewer,
        ] {
            assert_eq!(parse_agent(agent_name(a)), Some(a));
        }
        assert_eq!(parse_agent("nope"), None);
        assert_eq!(parse_agent("triage"), None, "триаж выведен (#1135)");
        for t in [
            TaskType::Bug,
            TaskType::Spike,
            TaskType::Docs,
            TaskType::Other,
            TaskType::Feature,
            TaskType::Refactoring,
            TaskType::Legacy,
            TaskType::Qa,
            TaskType::Infrastructure,
        ] {
            assert_eq!(parse_task_type(task_type_name(t)), Some(t));
        }
        // idea/analys выведены: старые значения — неизвестный тип
        assert_eq!(parse_task_type("idea"), None);
        assert_eq!(parse_task_type("analys"), None);
    }

    #[test]
    fn slot_keys_unique() {
        let keys = [
            slot_key(Slot::Design),
            slot_key(Slot::Dev),
            slot_key(Slot::PlanReview),
            slot_key(Slot::ResultReview),
            slot_key(Slot::Test),
            slot_key(Slot::TestReview),
        ];
        for i in 0..keys.len() {
            for j in i + 1..keys.len() {
                assert_ne!(keys[i], keys[j]);
            }
        }
    }
}
