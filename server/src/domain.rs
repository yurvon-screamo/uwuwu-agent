//! Домен Конвейера: статусы задач и маршрутизация тика.
//! Чистые функции без инфры — порт routeTask фабрики (spawn-сторона).

use crate::fm::Frontmatter;

/// Статусы канона досок uwuwu. Empty — строка без статуса
/// (легаси-черновик владельца: конвейер не трогает).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Draft,
    Todo,
    Design,
    ReviewPlan,
    InProgress,
    ReviewResult,
    Testing,
    UserReview,
    Done,
    NeedsAttention,
    Wait,
    Deleted,
    Empty,
}

impl Status {
    pub fn parse(raw: &str) -> Self {
        match raw.trim() {
            "Draft" => Self::Draft,
            "Todo" => Self::Todo,
            "Design" => Self::Design,
            "Ревью плана" => Self::ReviewPlan,
            "In Progress" => Self::InProgress,
            "Ревью результата" => Self::ReviewResult,
            "Тестирование" => Self::Testing,
            "Review" => Self::UserReview,
            "Done" => Self::Done,
            "Needs attention" => Self::NeedsAttention,
            "Вейт" => Self::Wait,
            "Deleted" => Self::Deleted,
            "" => Self::Empty,
            other => {
                tracing::warn!(event = "status.unknown", status = %other, "Unknown board status");
                Self::Empty
            }
        }
    }

    /// LLM-тратящий статус: занимает слот CUP.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Design | Self::ReviewPlan | Self::InProgress | Self::ReviewResult | Self::Testing
        )
    }
}

/// Слот сессии стадии (ячейки «Сессия dev/ревью/тест»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Design,
    Dev,
    PlanReview,
    ResultReview,
    Test,
}

impl Slot {
    pub fn fm_key(self) -> &'static str {
        match self {
            Self::Design => "design-session",
            Self::Dev => "dev-session",
            Self::PlanReview => "review-session",
            Self::ResultReview => "review-session",
            Self::Test => "test-session",
        }
    }
}

/// Срез строки доски, достаточный для маршрутизации.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub number: u64,
    pub title: String,
    pub status: Status,
    pub priority: Priority,
    pub autodone: bool,
    pub branch: Option<String>,
    pub pr: Option<String>,
    pub session: SessionMap,
}

/// Слоты → id сессий опенкода (из fm-зеркала).
pub type SessionMap = std::collections::BTreeMap<String, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Priority {
    High,
    Medium,
    Low,
    /// Не задан — ниже low в очереди CUP.
    #[default]
    Empty,
}

impl Priority {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "high" => Self::High,
            "medium" => Self::Medium,
            "low" => Self::Low,
            _ => Self::Empty,
        }
    }

    /// Пустой приоритет — ниже low (канон очереди CUP).
    pub fn rank(self) -> u8 {
        match self {
            Self::High => 0,
            Self::Medium => 1,
            Self::Low => 2,
            Self::Empty => 3,
        }
    }
}

/// Решение тика по одной строке.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Спавн агента стадии (сессии нет или мертва — решает тик по liveness).
    Spawn {
        number: u64,
        slot: Slot,
        reason: String,
    },
    /// Review + autodone: проба PR (мерж/Done/bounce).
    AutodoneProbe { number: u64 },
    /// Строка-наблюдатель: решения нет.
    Observe { number: u64, reason: String },
}

impl TaskRow {
    pub fn from_parts(number: u64, title: String, status_raw: &str, body: &str) -> Self {
        let fm = Frontmatter::parse(body);
        let mut session = SessionMap::new();
        for (slot, key) in [
            (Slot::Dev, "dev-session"),
            (Slot::PlanReview, "review-session"),
            (Slot::Test, "test-session"),
            (Slot::Design, "design-session"),
        ] {
            if let Some(v) = fm.get(key).filter(|v| !v.is_empty()) {
                session.insert(slot.fm_key().to_string(), v.to_string());
            }
        }
        Self {
            number,
            title,
            status: Status::parse(status_raw),
            priority: Priority::parse(fm.get("priority").unwrap_or_default()),
            autodone: fm.get("autodone").map(|v| v != "false").unwrap_or(false),
            branch: fm.get("branch").filter(|v| !v.is_empty()).map(String::from),
            pr: fm.get("pr").filter(|v| !v.is_empty()).map(String::from),
            session,
        }
    }

    pub fn session_of(&self, slot: Slot) -> Option<&str> {
        self.session.get(slot.fm_key()).map(|s| s.as_str())
    }

    /// Маршрут строки: какое действие тика назревает. Чистая функция:
    /// гейты CUP/пик/сталл — сверху (pipeline), здесь только строка.
    pub fn route(&self) -> Decision {
        let n = self.number;
        match self.status {
            Status::Design => match self.session_of(Slot::Design) {
                None => Decision::Spawn {
                    number: n,
                    slot: Slot::Design,
                    reason: "Design без сессии".into(),
                },
                Some(_) => Decision::Observe {
                    number: n,
                    reason: "Design: сессия заявлена".into(),
                },
            },
            Status::ReviewPlan => match self.session_of(Slot::PlanReview) {
                None => Decision::Spawn {
                    number: n,
                    slot: Slot::PlanReview,
                    reason: "Ревью плана без сессии".into(),
                },
                Some(_) => Decision::Observe {
                    number: n,
                    reason: "Ревью плана: сессия есть".into(),
                },
            },
            Status::InProgress => match self.session_of(Slot::Dev) {
                None => Decision::Spawn {
                    number: n,
                    slot: Slot::Dev,
                    reason: "In Progress без дев-сессии".into(),
                },
                Some(_) => Decision::Observe {
                    number: n,
                    reason: "In Progress: дев-сессия есть".into(),
                },
            },
            Status::ReviewResult => match self.session_of(Slot::ResultReview) {
                None => Decision::Spawn {
                    number: n,
                    slot: Slot::ResultReview,
                    reason: "Ревью результата без сессии".into(),
                },
                Some(_) => Decision::Observe {
                    number: n,
                    reason: "Ревью результата: сессия есть".into(),
                },
            },
            Status::Testing => match self.session_of(Slot::Test) {
                None => Decision::Spawn {
                    number: n,
                    slot: Slot::Test,
                    reason: "Тестирование без сессии".into(),
                },
                Some(_) => Decision::Observe {
                    number: n,
                    reason: "Тестирование: сессия есть".into(),
                },
            },
            Status::UserReview if self.autodone => Decision::AutodoneProbe { number: n },
            Status::UserReview => Decision::Observe {
                number: n,
                reason: "Review без автодона: мяч у владельца".into(),
            },
            Status::Draft => Decision::Observe {
                number: n,
                reason: "Draft: ждёт триаж".into(),
            },
            Status::Todo => Decision::Observe {
                number: n,
                reason: "Todo: ждёт Design владельцем".into(),
            },
            Status::Wait => Decision::Observe {
                number: n,
                reason: "Вейт: крон-мастер либо парковка".into(),
            },
            Status::Done | Status::Deleted => Decision::Observe {
                number: n,
                reason: "терминальный статус".into(),
            },
            Status::NeedsAttention => Decision::Observe {
                number: n,
                reason: "Needs attention: ждёт владельца".into(),
            },
            Status::Empty => Decision::Observe {
                number: n,
                reason: "пустой статус: легаси-черновик".into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(status: &str, body: &str) -> TaskRow {
        TaskRow::from_parts(7, "t".into(), status, body)
    }

    #[test]
    fn routes_spawn_statuses() {
        for (status, slot) in [
            ("Design", Slot::Design),
            ("Ревью плана", Slot::PlanReview),
            ("In Progress", Slot::Dev),
            ("Ревью результата", Slot::ResultReview),
            ("Тестирование", Slot::Test),
        ] {
            let d = row(status, "---\nstatus: x\n---\nтело").route();
            match d {
                Decision::Spawn { slot: s, .. } => assert_eq!(s, slot, "статус {status}"),
                other => panic!("статус {status}: ждали спавн, получили {other:?}"),
            }
        }
    }

    #[test]
    fn session_claim_blocks_spawn() {
        let body = "---\ndev-session: ses_1\n---\nтело";
        assert!(matches!(
            row("In Progress", body).route(),
            Decision::Observe { .. }
        ));
    }

    #[test]
    fn autodone_probe_only_with_flag() {
        let with = "---\nautodone: true\n---\nx";
        assert_eq!(
            row("Review", with).route(),
            Decision::AutodoneProbe { number: 7 }
        );
        let without = "---\nautodone: false\n---\nx";
        assert!(matches!(
            row("Review", without).route(),
            Decision::Observe { .. }
        ));
    }

    #[test]
    fn priority_rank_orders_queue() {
        assert!(Priority::High.rank() < Priority::Medium.rank());
        assert!(Priority::Low.rank() < Priority::Empty.rank());
    }

    #[test]
    fn cup_active_statuses() {
        assert!(Status::InProgress.is_active());
        assert!(!Status::UserReview.is_active());
        assert!(!Status::Empty.is_active());
    }
}
