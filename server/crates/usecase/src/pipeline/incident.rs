//! Инцидент-громкость сталл-детекта (#1281): массовость эскалаций за
//! скользящее окно — один громкий сигнал владельцу вместо N тихих
//! парковок (канал — ТГ, как у кронов UC-02 спеки).

use std::collections::VecDeque;

/// Порог инцидента: столько сталл-эскалаций за окно сигналят одним
/// сообщением. Данные инцидента 18.11 — 7 эскалаций за окно; порог 3
/// отсекает одиночные эскалации, ловит массовые.
pub const STALL_INCIDENT_THRESHOLD: usize = 3;
/// Окно скользящего счёта эскалаций.
pub const STALL_INCIDENT_WINDOW_MS: u64 = 60 * 60_000;
/// Анти-повтор: повторный сигнал не раньше, чем через cooldown.
pub const STALL_INCIDENT_COOLDOWN_MS: u64 = 60 * 60_000;

/// Счётчик скользящего окна сталл-эскалаций. Чистая политика без
/// носителя и сети: вызывающий регистрирует эскалации и по true отправляет
/// сигнал в свой канал.
#[derive(Debug)]
pub struct IncidentLoudness {
    threshold: usize,
    window_ms: u64,
    cooldown_ms: u64,
    escalations: VecDeque<u64>,
    last_signal_ms: Option<u64>,
}

impl IncidentLoudness {
    pub fn new(threshold: usize, window_ms: u64, cooldown_ms: u64) -> Self {
        Self {
            threshold,
            window_ms,
            cooldown_ms,
            escalations: VecDeque::new(),
            last_signal_ms: None,
        }
    }

    /// Регистрирует эскалацию в момент `at_ms`. true — порог инцидента
    /// превышен и cooldown истёк: сигналить. Эскалации старше окна
    /// забываются; сигнал обновляет cooldown.
    pub fn register(&mut self, at_ms: u64) -> bool {
        self.escalations.push_back(at_ms);
        let cutoff = at_ms.saturating_sub(self.window_ms);
        self.escalations.retain(|&t| t >= cutoff);
        if self.escalations.len() < self.threshold {
            return false;
        }
        if let Some(last) = self.last_signal_ms {
            if at_ms.saturating_sub(last) < self.cooldown_ms {
                return false;
            }
        }
        self.last_signal_ms = Some(at_ms);
        true
    }

    /// Эскалации в текущем окне — масштаб инцидента для текста сигнала.
    pub fn escalations_in_window(&self) -> usize {
        self.escalations.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60_000;

    #[test]
    fn below_threshold_is_quiet() {
        let mut l = IncidentLoudness::new(3, 60 * MIN, 60 * MIN);
        assert!(!l.register(0));
        assert!(
            !l.register(MIN),
            "две эскалации за окно — одиночный сталл, не инцидент"
        );
        assert_eq!(l.escalations_in_window(), 2);
    }

    #[test]
    fn threshold_crossing_signals_once() {
        let mut l = IncidentLoudness::new(3, 60 * MIN, 60 * MIN);
        l.register(0);
        l.register(MIN);
        assert!(l.register(2 * MIN), "третья эскалация в окне — инцидент");
        assert!(
            !l.register(3 * MIN),
            "cooldown держит: четвёртая эскалация не повторяет сигнал"
        );
    }

    #[test]
    fn scattered_escalations_outside_window_do_not_signal() {
        // эскалации, размазанные шире окна, не суммируются в инцидент
        let mut l = IncidentLoudness::new(3, 60 * MIN, 60 * MIN);
        assert!(!l.register(0));
        assert!(!l.register(30 * MIN));
        assert!(
            !l.register(61 * MIN),
            "первая эскалация забылась окном — инцидента нет"
        );
        assert_eq!(l.escalations_in_window(), 2);
    }

    #[test]
    fn signal_repeats_after_cooldown() {
        let mut l = IncidentLoudness::new(2, 60 * MIN, 60 * MIN);
        l.register(0);
        assert!(l.register(MIN), "первый сигнал");
        assert!(!l.register(30 * MIN));
        assert!(
            l.register(61 * MIN),
            "новая волна эскалаций после cooldown сигналит снова"
        );
    }
}
