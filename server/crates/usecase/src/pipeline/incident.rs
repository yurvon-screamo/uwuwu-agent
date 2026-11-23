//! Инцидент-громкость (#1281, #1382, #1412): массовость сигналов за
//! скользящее окно — один громкий сигнал владельцу вместо N тихих
//! парковок (канал — ТГ, как у кронов UC-02 спеки). Контуры: сталл-детект,
//! деградация исполнителя и давка CUP (преемпшен-гашения).

use std::collections::VecDeque;

/// Порог инцидента: столько эскалаций/гашений за окно сигналят одним
/// сообщением. Данные инцидента 18.11 — 7 эскалаций за окно; порог 3
/// отсекает одиночные эскалации, ловит массовые.
pub const INCIDENT_THRESHOLD: usize = 3;
/// Окно скользящего счёта эскалаций.
pub const INCIDENT_WINDOW_MS: u64 = 60 * 60_000;
/// Анти-повтор: повторный сигнал не раньше, чем через cooldown.
pub const INCIDENT_COOLDOWN_MS: u64 = 60 * 60_000;

/// Лимитная пауза (#1477): сигнал о начале окна исчерпания подписки.
/// Порог 1 — один строковый факт достаточен (пауза — не инцидент сбоя,
/// а норма контура, владельцу нужен факт начала окна); cooldown ≥ самого
/// длинного окна (~5 ч) — один сигнал на лимитное окно (L2 ревью плана).
pub const LIMIT_SIGNAL_THRESHOLD: usize = 1;
pub const LIMIT_SIGNAL_COOLDOWN_MS: u64 = 6 * 60 * 60_000;

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

    /// Лимитное окно (#1477): threshold 1 + cooldown ≥ окна — один сигнал
    /// на окно исчерпания подписки (L2 ревью плана).
    #[test]
    fn limit_window_signals_once_per_cooldown() {
        let mut l = IncidentLoudness::new(
            LIMIT_SIGNAL_THRESHOLD,
            INCIDENT_WINDOW_MS,
            LIMIT_SIGNAL_COOLDOWN_MS,
        );
        assert!(
            l.register(0),
            "threshold 1: первая лимитная пауза окно сигналит"
        );
        assert!(
            !l.register(2 * MIN),
            "повторные паузы в окне сигнал не дублируют"
        );
        assert!(
            !l.register(LIMIT_SIGNAL_COOLDOWN_MS - MIN),
            "до истечения cooldown повторного сигнала нет"
        );
        assert!(
            l.register(LIMIT_SIGNAL_COOLDOWN_MS + MIN),
            "новое окно после cooldown сигналит снова (ночь за ночью)"
        );
    }
}
