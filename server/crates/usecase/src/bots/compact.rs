//! Автокомпакт вечных сессий ботов — перенос ADR-082 на серверную эпоху
//! (#1221): чистая матрица вердикта. Компакцию делает нативный механизм
//! опенкода (POST /session/{id}/compact, durable admission — сам будит
//! idle-луп сессии); собственная механика — только расписание: тик
//! супервизора решает, когда стрелять. Триггер владельца (постановка
//! 05.11, ADR-082): idle ≥ 4ч И занятость окна ≥ 40% лимита модели,
//! коулдаун 1ч.

use serde::Serialize;

/// Политика автокомпакта: стрелять нативным compact по idle-сессии,
/// когда контекст раздут ≥ `min_context_pct` % лимита модели, не чаще
/// `cooldown_ms`. Значения — секция `[compact]` конфига сервера;
/// секция отсутствует — механика off (safe default).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CompactPolicy {
    pub idle_ms: u64,
    pub min_context_pct: f64,
    pub cooldown_ms: u64,
}

/// Факты сессии на момент тика (собирает супервизор через порты).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CompactFacts {
    /// Ход идёт прямо сейчас (`time.busy > time.idle`) — busy исключает
    /// fire (гвард активного хода; zombie-окно сторожит клиент скана).
    pub busy: bool,
    /// ms простоя сессии; None — граница не подтверждена (сообщений нет /
    /// сбой чтения) — не стреляем.
    pub idle_ms: Option<u64>,
    /// Занятость окна контекста, % лимита модели; None — не измерить
    /// (assistant-поворотов нет / лимит модели неизвестен).
    pub usage_pct: Option<f64>,
    /// ms последней адмиссии (0 — не было); окно коулдауна глушит.
    pub last_compact_ms: u64,
    pub now_ms: u64,
}

/// Вердикт матрицы: fire + reason (человекочитаемая причина — в лог).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompactVerdict {
    pub fire: bool,
    pub reason: String,
}

/// Вся матрица решения в одном чистом месте; reason — для лога
/// отказа/выстрела (наблюдаемость #1221).
pub fn compact_verdict(facts: &CompactFacts, policy: &CompactPolicy) -> CompactVerdict {
    let deny = |reason: &str| CompactVerdict {
        fire: false,
        reason: reason.to_string(),
    };
    if facts.busy {
        return deny("сессия занята");
    }
    let Some(idle) = facts.idle_ms else {
        return deny("idle не подтверждён");
    };
    if idle < policy.idle_ms {
        return deny(&format!(
            "idle {}м < {}м",
            idle / 60_000,
            policy.idle_ms / 60_000
        ));
    }
    let Some(usage) = facts.usage_pct else {
        return deny("контекст не измерен");
    };
    if usage < policy.min_context_pct {
        return deny(&format!(
            "контекст {}% < {}%",
            usage as u64, policy.min_context_pct as u64
        ));
    }
    if facts.now_ms.saturating_sub(facts.last_compact_ms) < policy.cooldown_ms {
        return deny("коулдаун прошлой адмиссии");
    }
    CompactVerdict {
        fire: true,
        reason: format!(
            "idle {:.1}ч, контекст {}%",
            idle as f64 / 3_600_000.0,
            usage as u64
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(idle_min: u64, pct: f64, cooldown_min: u64) -> CompactPolicy {
        CompactPolicy {
            idle_ms: idle_min * 60_000,
            min_context_pct: pct,
            cooldown_ms: cooldown_min * 60_000,
        }
    }

    fn facts(idle_min: u64, pct: f64) -> CompactFacts {
        CompactFacts {
            busy: false,
            idle_ms: Some(idle_min * 60_000),
            usage_pct: Some(pct),
            last_compact_ms: 0,
            now_ms: 10_000_000,
        }
    }

    #[test]
    fn fires_on_idle_and_usage() {
        let v = compact_verdict(&facts(240, 41.0), &policy(240, 40.0, 60));
        assert!(v.fire, "{:?}", v.reason);
        assert!(v.reason.contains("idle 4.0ч"));
        assert!(v.reason.contains("контекст 41%"));
    }

    #[test]
    fn busy_session_never_fires() {
        let mut f = facts(600, 90.0);
        f.busy = true;
        let v = compact_verdict(&f, &policy(240, 40.0, 60));
        assert!(!v.fire);
        assert_eq!(v.reason, "сессия занята");
    }

    #[test]
    fn unconfirmed_idle_never_fires() {
        let mut f = facts(600, 90.0);
        f.idle_ms = None;
        let v = compact_verdict(&f, &policy(240, 40.0, 60));
        assert!(!v.fire);
        assert_eq!(v.reason, "idle не подтверждён");
    }

    #[test]
    fn idle_below_threshold_denied() {
        let v = compact_verdict(&facts(239, 90.0), &policy(240, 40.0, 60));
        assert!(!v.fire);
        assert!(v.reason.contains("idle 239м < 240м"));
    }

    #[test]
    fn unmeasured_usage_denied() {
        let mut f = facts(600, 0.0);
        f.usage_pct = None;
        let v = compact_verdict(&f, &policy(240, 40.0, 60));
        assert!(!v.fire);
        assert_eq!(v.reason, "контекст не измерен");
    }

    #[test]
    fn usage_below_threshold_denied() {
        let v = compact_verdict(&facts(600, 39.9), &policy(240, 40.0, 60));
        assert!(!v.fire);
        assert!(v.reason.contains("39% < 40%"));
    }

    #[test]
    fn cooldown_suppresses_refire() {
        let mut f = facts(600, 90.0);
        f.last_compact_ms = f.now_ms - 30 * 60_000;
        let v = compact_verdict(&f, &policy(240, 40.0, 60));
        assert!(!v.fire);
        assert_eq!(v.reason, "коулдаун прошлой адмиссии");
        // коулдаун истёк — стреляем снова
        f.last_compact_ms = f.now_ms - 61 * 60_000;
        assert!(compact_verdict(&f, &policy(240, 40.0, 60)).fire);
    }

    #[test]
    fn zero_last_compact_means_never() {
        let mut f = facts(600, 90.0);
        f.last_compact_ms = 0;
        assert!(compact_verdict(&f, &policy(240, 40.0, 60)).fire);
    }
}
