//! Громкость деградации executor'а (#1312): N транспортных отказов
//! union-скана за скользящее окно — один громкий сигнал владельцу
//! per-executor. Канал и механика — инцидент-контур #1281: реюз
//! IncidentLoudness, второй алерт-механизм не заводится.

use std::collections::HashMap;

use super::incident::IncidentLoudness;

/// Порог: столько отказов executor'а за окно сигналят. Тик 180 c: живой
/// стейл-туннель сигналит за ~3 тика (≈9 мин); моргающие деградации ловит
/// окно — «за окно» вместо строго «подряд» (ревью плана #1312).
pub const EXECUTOR_UNAVAILABLE_THRESHOLD: usize = 3;
/// Окно скользящего счёта отказов.
pub const EXECUTOR_UNAVAILABLE_WINDOW_MS: u64 = 60 * 60_000;
/// Анти-повтор: повторный сигнал executor'а не раньше cooldown.
pub const EXECUTOR_UNAVAILABLE_COOLDOWN_MS: u64 = 60 * 60_000;

/// Один инцидент: executor + масштаб (отказы в окне) — поля текста сигнала.
pub struct ExecutorIncident {
    pub executor: String,
    pub failures_in_window: usize,
}

/// Per-executor громкость транспортных отказов: отказы одного executor'а
/// не смешиваются с другим — сигнал называет виновника. Неперсистентна
/// осознанно: «рестарт = свежее окно» (философия ADR-107, как у сталла
/// #1281); обработка продолжается при деградации (прецедент ADR-086).
pub struct ExecutorLoudness {
    per_executor: HashMap<String, IncidentLoudness>,
    threshold: usize,
    window_ms: u64,
    cooldown_ms: u64,
}

impl ExecutorLoudness {
    pub fn new(threshold: usize, window_ms: u64, cooldown_ms: u64) -> Self {
        Self {
            per_executor: HashMap::new(),
            threshold,
            window_ms,
            cooldown_ms,
        }
    }

    /// Регистрирует транспортный отказ executor'а в момент `at_ms`.
    /// Some — порог превышен и cooldown истёк: сигналить. Отказы старше
    /// окна забываются; успех executor'а регистрируется отсутствием вызова
    /// и окно не гасит (деградация, не строгий «подряд»).
    pub fn register_failure(&mut self, executor: &str, at_ms: u64) -> Option<ExecutorIncident> {
        let loudness = self
            .per_executor
            .entry(executor.to_string())
            .or_insert_with(|| {
                IncidentLoudness::new(self.threshold, self.window_ms, self.cooldown_ms)
            });
        if !loudness.register(at_ms) {
            return None;
        }
        Some(ExecutorIncident {
            executor: executor.to_string(),
            failures_in_window: loudness.escalations_in_window(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60_000;

    #[test]
    fn two_failures_below_threshold_are_quiet() {
        let mut l = ExecutorLoudness::new(3, 60 * MIN, 60 * MIN);
        assert!(l.register_failure("kisa", 0).is_none());
        assert!(
            l.register_failure("kisa", MIN).is_none(),
            "два отказа за окно — одиночный сбой, не инцидент"
        );
    }

    #[test]
    fn third_failure_within_window_signals_with_executor_name() {
        let mut l = ExecutorLoudness::new(3, 60 * MIN, 60 * MIN);
        l.register_failure("kisa", 0);
        l.register_failure("kisa", MIN);
        let incident = l.register_failure("kisa", 2 * MIN).expect("порог достигнут");
        assert_eq!(incident.executor, "kisa");
        assert_eq!(incident.failures_in_window, 3);
    }

    #[test]
    fn failures_of_one_executor_do_not_signal_another() {
        let mut l = ExecutorLoudness::new(3, 60 * MIN, 60 * MIN);
        l.register_failure("kisa", 0);
        l.register_failure("kisa", MIN);
        assert!(
            l.register_failure("redmi", 2 * MIN).is_none(),
            "смешение отказов разных executor'ов дало бы ложный виновник"
        );
        l.register_failure("redmi", 3 * MIN);
        let incident = l
            .register_failure("redmi", 4 * MIN)
            .expect("redmi достигает порога собственными отказами");
        assert_eq!(incident.executor, "redmi");
    }

    #[test]
    fn repeat_signal_held_by_cooldown_then_released() {
        let mut l = ExecutorLoudness::new(3, 60 * MIN, 60 * MIN);
        for i in 0..3 {
            l.register_failure("kisa", i * MIN);
        }
        assert!(l.register_failure("kisa", 4 * MIN).is_none(), "анти-повтор");
        assert!(
            l.register_failure("kisa", 70 * MIN).is_none(),
            "старый раунд выветрился из окна — счёт заново"
        );
        assert!(l.register_failure("kisa", 80 * MIN).is_none());
        assert!(
            l.register_failure("kisa", 90 * MIN).is_some(),
            "после cooldown новый раунд сигналит"
        );
    }

    #[test]
    fn failures_spread_across_window_still_signal() {
        // деградация, не строгий «подряд»: разрозненные отказы в пределах
        // окна (тихие тики между ними) — тот же инцидент (ревью #1312).
        let mut l = ExecutorLoudness::new(3, 60 * MIN, 60 * MIN);
        assert!(l.register_failure("kisa", 0).is_none());
        assert!(l.register_failure("kisa", 25 * MIN).is_none());
        assert!(l.register_failure("kisa", 50 * MIN).is_some());
    }
}
