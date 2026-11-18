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
    /// Автомерж: проба PR → мерж (строка остаётся в Review) / bounce (#1232).
    Automerge,
    /// Строка-наблюдение: чего она ждёт по канону.
    Observe { awaits: Awaits },
}

/// Семантика ожидания строки (канон «кто держит мяч»): без слов,
/// словами это скажет слой логирования.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Awaits {
    /// Стадия ведётся живой сессией (мяч у агента стадии).
    StageSession,
    /// Мяч у владельца: приёмка Review, Needs attention, черновик Draft и
    /// проработанный Todo (выбор Design / fast-track).
    Owner,
    /// Вход в конвейер: Analys ждёт аналитика, Review Analys — ревьювера
    /// аналитики.
    Entry,
    /// Крон-мастер либо парковка (Вейт).
    Schedule,
    /// Терминальный статус (Done/Deleted).
    Terminal,
    /// Без статуса: легаси-черновик, конвейер не трогает.
    Untracked,
}

/// Чего ждёт строка вне спавна/автомержа.
pub fn awaits(status: Status) -> Awaits {
    match status {
        Status::Design
        | Status::ReviewPlan
        | Status::InProgress
        | Status::ReviewResult
        | Status::Testing => Awaits::StageSession,
        Status::UserReview | Status::NeedsAttention => Awaits::Owner,
        Status::Draft => Awaits::Owner,
        Status::Todo => Awaits::Owner,
        Status::Analys | Status::ReviewAnalys => Awaits::Entry,
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
    if task.automerge_ready() {
        return Action::Automerge;
    }
    Action::Observe {
        awaits: awaits(task.status()),
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
    if policy.peak.holds(now) && task.priority() != Priority::High {
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

/// Attention-механика (директива владельца 15.11): спавн слота при
/// неизменном статусе строки — повтор; третий повтор = петля, строка
/// эскалируется владельцу (Needs attention), конвейер её бросает.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnLoop {
    status: Status,
    repeats: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopOutcome {
    /// Спавн допустим.
    Continue,
    /// Петля: Needs attention + коммент, спавн запрещён.
    Escalate,
}

impl SpawnLoop {
    pub fn new(status: Status) -> Self {
        Self { status, repeats: 0 }
    }

    /// Зарегистрировать очередной спавн слота; смена статуса строки
    /// (прогресс конвейера) обнуляет счётчик.
    pub fn register_spawn(&mut self, current: Status) -> LoopOutcome {
        if current != self.status {
            self.status = current;
            self.repeats = 0;
        }
        self.repeats += 1;
        // Порог 4 (владелец 16.11): сложные задачи легитимно ходят кругами —
        // >4 повторов без прогресса статуса = петля
        if self.repeats > 4 {
            LoopOutcome::Escalate
        } else {
            LoopOutcome::Continue
        }
    }

    /// Текущий счётчик повторов.
    pub fn repeats(&self) -> u32 {
        self.repeats
    }

    /// Статус, при котором считали.
    pub fn status(&self) -> Status {
        self.status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(status: Status) -> Task {
        Task::new(7, "t".into(), status)
    }

    #[test]
    fn spawn_loop_escalates_after_four_repeats() {
        // Порог владельца 16.11: >4 повторов без прогресса = петля;
        // сложные задачи легитимно ходят кругами до 4 включительно
        let mut g = SpawnLoop::new(Status::InProgress);
        for _ in 0..4 {
            assert_eq!(g.register_spawn(Status::InProgress), LoopOutcome::Continue);
        }
        assert_eq!(g.register_spawn(Status::InProgress), LoopOutcome::Escalate);
        // прогресс статуса обнуляет счётчик: смена статуса регистрируется
        // как первый повтор в новом статусе
        let mut g2 = SpawnLoop::new(Status::InProgress);
        g2.register_spawn(Status::InProgress);
        g2.register_spawn(Status::InProgress);
        assert_eq!(
            g2.register_spawn(Status::ReviewResult),
            LoopOutcome::Continue
        );
        assert_eq!(
            g2.repeats(),
            1,
            "смена статуса = первый повтор нового раунда"
        );
    }

    #[test]
    fn spawn_loop_progress_resets() {
        let mut guard = SpawnLoop::new(Status::ReviewResult);
        guard.register_spawn(Status::ReviewResult);
        guard.register_spawn(Status::ReviewResult);
        // статус поехал (прогресс конвейера) — счётчик с нуля;
        // порог 4: эскалация на пятом повторе нового раунда
        assert_eq!(guard.register_spawn(Status::Testing), LoopOutcome::Continue);
        assert_eq!(guard.repeats(), 1);
        assert_eq!(guard.register_spawn(Status::Testing), LoopOutcome::Continue);
        assert_eq!(guard.register_spawn(Status::Testing), LoopOutcome::Continue);
        assert_eq!(guard.register_spawn(Status::Testing), LoopOutcome::Continue);
        assert_eq!(guard.register_spawn(Status::Testing), LoopOutcome::Escalate);
    }

    #[test]
    fn spawn_loop_tracks_status_and_repeats() {
        let mut guard = SpawnLoop::new(Status::ReviewResult);
        guard.register_spawn(Status::ReviewResult);
        assert_eq!(guard.repeats(), 1);
        assert_eq!(guard.status(), Status::ReviewResult);
        guard.register_spawn(Status::ReviewResult);
        assert_eq!(guard.repeats(), 2);
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
        t.claim(Slot::Dev, crate::task::SessionRef::parse("ses_1").unwrap())
            .unwrap();
        assert!(matches!(decide(&t), Action::Observe { .. }));
    }

    #[test]
    fn decide_automerge_only_with_flag() {
        let mut t = task(Status::UserReview);
        let branch = crate::task::BranchName::parse("work/x").unwrap();
        t.apply_fields(crate::task::TaskFields::new(
            std::collections::BTreeMap::new(),
            crate::task::Vcs::Branch(branch),
            crate::task::Priority::Empty,
        ));
        assert!(matches!(decide(&t), Action::Observe { .. }));
        t.set_automerge(true);
        assert_eq!(decide(&t), Action::Automerge);
    }

    #[test]
    fn decide_automerge_needs_mergeable_carrier() {
        // docs-only (нет PR и ветки) мержить нечего: строка ждёт владельца,
        // тик не прощупывает (#1232); безветочный артефакт-URL (#1155) —
        // тот же класс «нечего мержить», не гейт мержа
        let mut t = task(Status::UserReview);
        t.set_automerge(true);
        assert!(matches!(decide(&t), Action::Observe { .. }));
        t.apply_fields(crate::task::TaskFields::new(
            std::collections::BTreeMap::new(),
            crate::task::Vcs::Artifact {
                url: crate::task::PrUrl::parse("https://github.com/a/b/blob/main/docs/x.md")
                    .unwrap(),
            },
            crate::task::Priority::Empty,
        ));
        assert!(
            matches!(decide(&t), Action::Observe { .. }),
            "Artifact — не мерж-гейт"
        );
        let branch = crate::task::BranchName::parse("work/x").unwrap();
        t.apply_fields(crate::task::TaskFields::new(
            std::collections::BTreeMap::new(),
            crate::task::Vcs::Branch(branch),
            crate::task::Priority::Empty,
        ));
        assert_eq!(decide(&t), Action::Automerge);
    }

    #[test]
    fn awaits_semantics_of_analysis_stage() {
        // #1135: Draft/Todo — мяч владельца; Analys/Review Analys — вход
        // в конвейер (очередь сайдкара аналитики)
        assert_eq!(awaits(Status::Draft), Awaits::Owner);
        assert_eq!(awaits(Status::Todo), Awaits::Owner);
        assert_eq!(awaits(Status::Analys), Awaits::Entry);
        assert_eq!(awaits(Status::ReviewAnalys), Awaits::Entry);
    }

    #[test]
    fn analysis_statuses_are_not_active_and_have_no_slot() {
        // территория сайдкара: CUP и пик-окно не задеваются, спавн — не тик
        assert!(!Status::Analys.is_active());
        assert!(!Status::ReviewAnalys.is_active());
        assert_eq!(task(Status::Analys).stage_slot(), None);
        assert_eq!(task(Status::ReviewAnalys).stage_slot(), None);
        assert!(matches!(
            decide(&task(Status::Analys)),
            Action::Observe { .. }
        ));
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
        high.set_priority(Priority::High);
        assert_eq!(
            gate(&high, &decide(&high), policy, 0, in_peak),
            Gate::Allowed
        );
        assert_eq!(
            gate(&t, &Action::Automerge, policy, 2, in_peak),
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
}
