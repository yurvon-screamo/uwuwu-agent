//! Глобальный спавн-бюджет: не более N сессий/час на весь сервер
//! (директива владельца 15.11 — предохранитель машинного темпа).
//! cap=0 — kill-switch: все спавны запрещены. Скользящее окно по
//! timestamps, чистка хвоста при каждом регистре.

/// Бюджет спавнов за скользящий час.
#[derive(Debug, Clone)]
pub struct SpawnBudget {
    cap: usize,
    window_ms: u64,
    stamps: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetVerdict {
    Allowed,
    /// Потолок исчерпан (или kill-switch) — спавн запрещён.
    Exhausted {
        cap: usize,
        used: usize,
    },
}

impl SpawnBudget {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            window_ms: 3_600_000,
            stamps: Vec::new(),
        }
    }

    /// Kill-switch (cap=0): спавны выключены одной переменной окружения.
    pub fn is_kill_switch(&self) -> bool {
        self.cap == 0
    }

    /// Проверить окно без регистрации (для логов/гейтов).
    pub fn peek(&self, now_ms: u64) -> BudgetVerdict {
        if self.cap == 0 {
            return BudgetVerdict::Exhausted { cap: 0, used: 0 };
        }
        let used = self.window(now_ms).count();
        if used >= self.cap {
            BudgetVerdict::Exhausted {
                cap: self.cap,
                used,
            }
        } else {
            BudgetVerdict::Allowed
        }
    }

    /// Зарегистрировать спавн: Allowed — записан, Exhausted — отказ (не записан).
    pub fn register(&mut self, now_ms: u64) -> BudgetVerdict {
        let verdict = self.peek(now_ms);
        if matches!(verdict, BudgetVerdict::Allowed) {
            self.stamps
                .retain(|ts| now_ms.saturating_sub(*ts) < self.window_ms);
            self.stamps.push(now_ms);
        }
        verdict
    }

    /// Сколько спавнов в активном окне.
    pub fn used(&self, now_ms: u64) -> usize {
        self.window(now_ms).count()
    }

    fn window(&self, now_ms: u64) -> impl Iterator<Item = &u64> {
        self.stamps
            .iter()
            .filter(move |ts| now_ms.saturating_sub(**ts) < self.window_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_exhausts_and_frees() {
        let mut b = SpawnBudget::new(2);
        assert!(matches!(b.register(1000), BudgetVerdict::Allowed));
        assert!(matches!(b.register(2000), BudgetVerdict::Allowed));
        assert_eq!(
            b.register(3000),
            BudgetVerdict::Exhausted { cap: 2, used: 2 },
            "потолок исчерпан — спавн не записан"
        );
        assert_eq!(b.used(3000), 2, "отказ не съедает слот");
        // окно сползло: штамп 1000 старше часа (час истёк в 3_601_000)
        assert!(matches!(b.register(3_602_000), BudgetVerdict::Allowed));
    }

    #[test]
    fn zero_is_kill_switch() {
        let mut b = SpawnBudget::new(0);
        assert!(b.is_kill_switch());
        assert_eq!(b.register(1), BudgetVerdict::Exhausted { cap: 0, used: 0 });
    }
}
