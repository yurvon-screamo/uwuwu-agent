//! Конвейер: доменная служба над задачами — решение тика, гейты
//! спавн-политики (CUP/пик), круги ревью. Состояние живёт в задачах
//! и ReviewRounds; правила — здесь. Чистые функции.

use crate::task::{Priority, Slot, Status, Task};

/// Минуты суток UTC + день недели (1..=7, понедельник = 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtcMinute {
    pub minute_of_day: u32,
    pub weekday: u32,
}

/// Пик-окно GLM: будни [start, end) минут UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeakWindow {
    pub enabled: bool,
    pub start_min: u32,
    pub end_min: u32,
}

impl PeakWindow {
    pub fn holds(self, now: UtcMinute) -> bool {
        self.enabled
            && now.weekday <= 5
            && now.minute_of_day >= self.start_min
            && now.minute_of_day < self.end_min
    }
}

/// Спавн-политика конвейера: кап активных LLM-задач + пик-окно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnPolicy {
    pub cup: usize,
    pub peak: PeakWindow,
}

/// Решение тика по одной задаче. Диагностические строки — не здесь:
/// слой исполнения (сервер) логирует своими словами по доменной семантике.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Спавн агента стадии (сессии нет).
    Spawn { slot: Slot },
    /// Автодон: проба PR → мерж/Done/bounce.
    Autodone,
    /// Строка-наблюдение: чего она ждёт по канону.
    Observe { awaits: Awaits },
}

/// Семантика ожидания строки (канон «кто держит мяч»): без слов,
/// словами это скажет слой логирования.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Awaits {
    /// Стадия ведётся живой сессией (мяч у агента стадии).
    StageSession,
    /// Мяч у владельца: приёмка Review, Needs attention, выбор Design.
    Owner,
    /// Вход в конвейер: Draft ждёт триаж, Todo ждёт Design.
    Entry,
    /// Крон-мастер либо парковка (Вейт).
    Schedule,
    /// Терминальный статус (Done/Deleted).
    Terminal,
    /// Без статуса: легаси-черновик, конвейер не трогает.
    Untracked,
}

/// Чего ждёт строка вне спавна/автодона.
pub fn awaits(status: Status) -> Awaits {
    match status {
        Status::Design
        | Status::ReviewPlan
        | Status::InProgress
        | Status::ReviewResult
        | Status::Testing => Awaits::StageSession,
        Status::UserReview | Status::NeedsAttention => Awaits::Owner,
        Status::Draft | Status::Todo => Awaits::Entry,
        Status::Wait => Awaits::Schedule,
        Status::Done | Status::Deleted => Awaits::Terminal,
        Status::Empty => Awaits::Untracked,
    }
}

/// Вердикт гейта спавн-политики.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Allowed,
    /// Очередь CUP: активных ≥ капа.
    HeldCup {
        active: usize,
        cap: usize,
    },
    /// Пик-окно: не-high заморожен до конца окна.
    DeferredPeak,
}

/// Решение по задаче: какой актёр стадии нужен тикум.
pub fn decide(task: &Task) -> Action {
    if let Some(slot) = task.stage_needs_spawn() {
        return Action::Spawn { slot };
    }
    if task.autodone_ready() {
        return Action::Autodone;
    }
    Action::Observe {
        awaits: awaits(task.status),
    }
}

/// Гейт спавна: пик-окно держит не-high, CUP держит сверх капа.
pub fn gate(
    task: &Task,
    action: &Action,
    policy: SpawnPolicy,
    active: usize,
    now: UtcMinute,
) -> Gate {
    let Action::Spawn { .. } = action else {
        return Gate::Allowed;
    };
    if policy.peak.holds(now) && task.priority != Priority::High {
        return Gate::DeferredPeak;
    }
    if active >= policy.cup {
        return Gate::HeldCup {
            active,
            cap: policy.cup,
        };
    }
    Gate::Allowed
}

/// Круги ревью задачи: инварианты канона — approve-with-notes ≤ 1 на задачу,
/// третий круг тех же замечаний → тупик владельцу.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReviewRounds {
    pub request_changes_rounds: u32,
    pub approve_with_notes_used: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundOutcome {
    /// Круг зачтён, конвейер продолжается.
    Continue,
    /// Тупик: эскалация владельцу (Needs attention).
    Escalate,
}

impl ReviewRounds {
    /// request_changes от ревьювера: 3 подряд → эскалация.
    pub fn register_request_changes(&mut self) -> RoundOutcome {
        self.request_changes_rounds += 1;
        if self.request_changes_rounds >= 3 {
            return RoundOutcome::Escalate;
        }
        RoundOutcome::Continue
    }

    /// Прочие вердикты сбрасывают счётчик одних и тех же замечаний.
    pub fn register_progress(&mut self) {
        self.request_changes_rounds = 0;
    }

    /// approve-with-notes выдаётся один раз на задачу.
    pub fn register_approve_with_notes(&mut self) -> Result<(), &'static str> {
        if self.approve_with_notes_used {
            return Err("approve-with-notes уже выдавался на эту задачу");
        }
        self.approve_with_notes_used = true;
        self.register_progress();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(status: Status) -> Task {
        Task::new(7, "t".into(), status)
    }

    #[test]
    fn decide_spawns_stage_without_session() {
        for status in [
            Status::Design,
            Status::ReviewPlan,
            Status::InProgress,
            Status::ReviewResult,
            Status::Testing,
        ] {
            let Action::Spawn { .. } = decide(&task(status)) else {
                panic!("{status:?}: ждали спавн")
            };
        }
    }

    #[test]
    fn decide_respects_claimed_session() {
        let mut t = task(Status::InProgress);
        t.claim(Slot::Dev, "ses_1".into());
        assert!(matches!(decide(&t), Action::Observe { .. }));
    }

    #[test]
    fn decide_autodone_only_with_flag() {
        let mut t = task(Status::UserReview);
        assert!(matches!(decide(&t), Action::Observe { .. }));
        t.autodone = true;
        assert_eq!(decide(&t), Action::Autodone);
    }

    #[test]
    fn gate_holds_cup_and_peak() {
        let policy = SpawnPolicy {
            cup: 2,
            peak: PeakWindow {
                enabled: true,
                start_min: 360,
                end_min: 600,
            },
        };
        let in_peak = UtcMinute {
            minute_of_day: 540,
            weekday: 3,
        };
        let off_peak = UtcMinute {
            minute_of_day: 100,
            weekday: 3,
        };
        let t = task(Status::InProgress);
        let spawn = decide(&t);

        assert_eq!(gate(&t, &spawn, policy, 0, in_peak), Gate::DeferredPeak);
        assert_eq!(
            gate(&t, &spawn, policy, 2, off_peak),
            Gate::HeldCup { active: 2, cap: 2 }
        );

        let mut high = task(Status::InProgress);
        high.priority = Priority::High;
        assert_eq!(
            gate(&high, &decide(&high), policy, 0, in_peak),
            Gate::Allowed
        );
        assert_eq!(
            gate(&t, &Action::Autodone, policy, 2, in_peak),
            Gate::Allowed
        );
    }

    #[test]
    fn peak_weekday_and_bounds() {
        let peak = PeakWindow {
            enabled: true,
            start_min: 360,
            end_min: 600,
        };
        assert!(peak.holds(UtcMinute {
            minute_of_day: 360,
            weekday: 1
        }));
        assert!(peak.holds(UtcMinute {
            minute_of_day: 599,
            weekday: 5
        }));
        assert!(!peak.holds(UtcMinute {
            minute_of_day: 600,
            weekday: 3
        }));
        assert!(!peak.holds(UtcMinute {
            minute_of_day: 540,
            weekday: 6
        }));
        let off = PeakWindow {
            enabled: false,
            start_min: 0,
            end_min: 1440,
        };
        assert!(!off.holds(UtcMinute {
            minute_of_day: 540,
            weekday: 3
        }));
    }

    #[test]
    fn review_rounds_invariants() {
        let mut r = ReviewRounds::default();
        assert_eq!(r.register_request_changes(), RoundOutcome::Continue);
        assert_eq!(r.register_request_changes(), RoundOutcome::Continue);
        assert_eq!(r.register_request_changes(), RoundOutcome::Escalate);
        r.register_progress();
        assert_eq!(
            r.register_request_changes(),
            RoundOutcome::Continue,
            "прогресс сбросил счётчик"
        );
        assert!(r.register_approve_with_notes().is_ok());
        assert!(
            r.register_approve_with_notes().is_err(),
            "второй approve-with-notes запрещён"
        );
    }
}
