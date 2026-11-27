//! Naming: маппинг доменных значений ↔ канонические имена носителей
//! (доска/опенкод/поля issue/state.json). Имена — конфигурация платформы,
//! не бизнес-логика: домен не знает, как он называется снаружи. Меняется
//! носитель — меняется этот модуль, домен нет.

use uwuwu_domain::{Agent, Complexity, Priority, Schedule, ScheduleKind, Slot, Status, TaskType};

/// Все валидные имена статуса на носителе: канон первым (запись/lookup
/// предпочитает его), легаси-русские — чтение/резолв досок до rename
/// опций (контракт окна merge→rename #1383, прецедент #1280 Autodone).
pub fn status_names(s: Status) -> &'static [&'static str] {
    match s {
        Status::Draft => &["Draft"],
        Status::Analys => &["Analys"],
        Status::ReviewAnalys => &["Review Analys"],
        Status::Todo => &["Todo"],
        Status::Design => &["Design"],
        Status::ReviewPlan => &["Plan review", "Ревью плана"],
        Status::InProgress => &["In Progress"],
        Status::ReviewResult => &["Result review", "Ревью результата"],
        Status::Testing => &["Testing", "Тестирование"],
        Status::ReviewTesting => &["Testing review", "Ревью тестирования"],
        Status::UserReview => &["Review"],
        Status::Done => &["Done"],
        Status::NeedsAttention => &["Needs attention"],
        Status::Wait => &["Wait", "Вейт"],
        Status::Deleted => &["Deleted"],
        Status::Empty => &[],
    }
}

/// Каноническое имя статуса (колонка доски) — первый из status_names.
pub fn status_name(s: Status) -> &'static str {
    status_names(s).first().copied().unwrap_or("")
}

/// Парсинг статуса с доски. Неизвестное имя → Empty: строка выпадает из
/// рабочих списков (легаси/мусор); логирование сырых имён — дело инфры.
/// Принимает канон и легаси-русские имена (окно merge→rename #1383).
pub fn parse_status(raw: &str) -> Status {
    match raw.trim() {
        "Draft" => Status::Draft,
        "Analys" => Status::Analys,
        "Review Analys" => Status::ReviewAnalys,
        "Todo" => Status::Todo,
        "Design" => Status::Design,
        "Plan review" | "Ревью плана" => Status::ReviewPlan,
        "In Progress" => Status::InProgress,
        "Result review" | "Ревью результата" => Status::ReviewResult,
        "Testing" | "Тестирование" => Status::Testing,
        "Testing review" | "Ревью тестирования" => Status::ReviewTesting,
        "Review" => Status::UserReview,
        "Done" => Status::Done,
        "Needs attention" => Status::NeedsAttention,
        "Wait" | "Вейт" => Status::Wait,
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
/// Имя типа в нативном словаре issue Type репо-хаба (заведён владельцем
/// 12.11: дефолты Task/Bug/Feature + остальные канонные 1:1) — запись
/// create/edit и чтение носителя одним знанием (#1271). Обратный маппинг
/// — parse_task_type (case-insensitive): «Task» (нативный дефолт),
/// null и неизвестное имя → None без громкой ошибки.
pub fn task_type_native_name(t: TaskType) -> &'static str {
    match t {
        TaskType::Bug => "Bug",
        TaskType::Feature => "Feature",
        t => task_type_name(t),
    }
}

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
/// Канон английский (#1383); физические поля с легаси-лейблом «Приоритет»
/// резолвятся на границе носителя (fields.rs resolve_field).
pub const F_PRIORITY: &str = "Priority";

/// Единое имя поля автомержа (#1232): записывается create/task_edit.
/// Legacy «Autodone» резолвится на границе носителя (fields.rs store,
/// item_values, parse_field_values — read-нормализация #1280) — сервер
/// читает/пишет оба имени, наружу всегда новое.
pub const F_AUTOMERGE: &str = "Automerge";

/// Единое имя поля direct (#1357): пишется только create (флаг —
/// свойство рождения строки, task_edit его не принимает).
pub const F_DIRECT: &str = "Direct";

/// Единое имя поля fact_track (#1631): класс строк «без Design и без
/// тестирования, только In Progress → Result review → Review».
/// Как direct: пишется только create (свойство рождения, immutable),
/// task_edit его не принимает (чужой ключ — громкий отказ #1572).
pub const F_FACT_TRACK: &str = "FactTrack";

/// Имя нативного поля issue со сложностью задачи (#1689). Опции
/// single_select — `regular|high|low`; русские формы принимает парсер.
pub const F_COMPLEXITY: &str = "Complexity";

/// Каноническое имя сложности (опция issue-поля и аргумент MCP).
pub fn complexity_name(complexity: Complexity) -> &'static str {
    match complexity {
        Complexity::High => "high",
        Complexity::Regular => "regular",
        Complexity::Low => "low",
    }
}

/// Строгий разбор сложности: канон и русские алиасы, регистр и пробелы
/// не влияют. Пустое и неизвестное — ошибка: граница MCP обязана
/// отклонять неканонические значения, сайты чтения различают пустое
/// и мусор по `Err` и исходной строке (#1689).
pub fn parse_complexity_strict(raw: &str) -> Result<Complexity, String> {
    match raw.trim().to_lowercase().as_str() {
        "high" | "высокая" => Ok(Complexity::High),
        "regular" | "общая" => Ok(Complexity::Regular),
        "low" | "низкая" => Ok(Complexity::Low),
        _ => Err(format!("неизвестная сложность '{raw}' (regular|high|low)")),
    }
}

/// Мягкое чтение носителя: пустое/неизвестное → Regular. Не испускает
/// событий — сайты с номером задачи вызывают strict сами и логируют
/// только непустой мусор (#1689).
pub fn parse_complexity(raw: &str) -> Complexity {
    parse_complexity_strict(raw).unwrap_or(Complexity::Regular)
}

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
        ScheduleKind::Interval(m) => format!("every {} min", m.get()),
        ScheduleKind::Daily(at) => format!(
            "daily {:02}:{:02} JST",
            at.minute_of_day() / 60,
            at.minute_of_day() % 60
        ),
        ScheduleKind::Once { at, on } => {
            let (day, month) = on.parts();
            format!(
                "once {:02}:{:02} {day:02}.{month:02}",
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
    use uwuwu_domain::{IntervalMinutes, JstTime};

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
    fn status_canon_names_are_english() {
        assert_eq!(status_name(Status::ReviewPlan), "Plan review");
        assert_eq!(status_name(Status::ReviewResult), "Result review");
        assert_eq!(status_name(Status::Testing), "Testing");
        assert_eq!(status_name(Status::Wait), "Wait");
    }

    #[test]
    fn legacy_status_names_parse_and_resolve() {
        // легаси-русские имена валидны на чтении/резолве до rename опций
        // досок (контракт окна merge→rename #1383); запись — всегда канон
        assert_eq!(parse_status("Ревью плана"), Status::ReviewPlan);
        assert_eq!(parse_status("Ревью результата"), Status::ReviewResult);
        assert_eq!(parse_status("Тестирование"), Status::Testing);
        assert_eq!(parse_status("Вейт"), Status::Wait);
        assert_eq!(parse_status(" Ревью плана "), Status::ReviewPlan);
        // канон первым: lookup предпочитает английский, легаси — запасной
        assert_eq!(
            status_names(Status::ReviewPlan),
            ["Plan review", "Ревью плана"]
        );
        assert_eq!(
            status_names(Status::ReviewResult),
            ["Result review", "Ревью результата"]
        );
        assert_eq!(status_names(Status::Testing), ["Testing", "Тестирование"]);
        assert_eq!(status_names(Status::Wait), ["Wait", "Вейт"]);
        // статусы без легаси — одиночное имя
        assert_eq!(status_names(Status::Todo), ["Todo"]);
        assert_eq!(status_names(Status::Empty), [] as [&str; 0]);
    }

    #[test]
    fn priority_field_canon_english() {
        assert_eq!(F_PRIORITY, "Priority");
    }

    #[test]
    fn schedule_label_is_english() {
        let every = schedule_label(&Schedule::new(ScheduleKind::Interval(
            IntervalMinutes::new(5).expect("valid interval"),
        )));
        assert_eq!(every, "every 5 min");
        let daily = schedule_label(&Schedule::new(ScheduleKind::Daily(
            JstTime::new(9, 0).expect("valid time"),
        )));
        assert_eq!(daily, "daily 09:00 JST");
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
            // круг через нативный словарь issue Type (#1271; tester #1383:
            // ассерты восстановлены из master — функция фиксапа ребейза
            // осталась без тестов)
            assert_eq!(parse_task_type(task_type_native_name(t)), Some(t));
        }
        // нативный дефолт Task и нативная капс канона читаются честно:
        // «Task» — не канонный тип (None), капс гасится lowercase-парсом
        assert_eq!(parse_task_type("Task"), None);
        assert_eq!(parse_task_type("Bug"), Some(TaskType::Bug));
        assert_eq!(task_type_native_name(TaskType::Bug), "Bug");
        // idea/analys выведены: старые значения — неизвестный тип
        assert_eq!(parse_task_type("idea"), None);
        assert_eq!(parse_task_type("analys"), None);
    }

    #[test]
    fn complexity_parsing_distinguishes_empty_from_garbage() {
        assert_eq!(parse_complexity_strict("high"), Ok(Complexity::High));
        assert_eq!(parse_complexity_strict(" Высокая "), Ok(Complexity::High));
        assert_eq!(parse_complexity_strict("regular"), Ok(Complexity::Regular));
        assert_eq!(parse_complexity_strict("общая"), Ok(Complexity::Regular));
        assert_eq!(parse_complexity_strict("низкая"), Ok(Complexity::Low));
        assert!(parse_complexity_strict("").is_err());
        assert!(parse_complexity_strict("мусор").is_err());
        assert_eq!(parse_complexity(""), Complexity::Regular);
        assert_eq!(parse_complexity("мусор"), Complexity::Regular);
        assert_eq!(complexity_name(Complexity::High), "high");
        assert_eq!(complexity_name(Complexity::Regular), "regular");
        assert_eq!(complexity_name(Complexity::Low), "low");
        assert_eq!(F_COMPLEXITY, "Complexity");
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
