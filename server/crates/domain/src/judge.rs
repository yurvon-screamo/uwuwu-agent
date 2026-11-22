//! Судья конвейера (#1364): типы вердикта decision-модели и чистые правила
//! применения. Судья возвращает структурированный choice из фиксированного
//! набора — конвейер читает структуру, не тексты агентов (узкая ревизия
//! ADR-106 по санкции владельца 20.11); каноничность перехода гардируется
//! здесь, а не формулировками промпта.

use crate::task::Status;

/// Статус-выбор судьи: фиксированный набор для всех строк — модель видит
/// текущий статус в контексте, каноничность перехода гардирует сервер
/// (решение ревью плана #1364, C1-а). Hold — «ничего не делать на этом
/// тике». Done/UserReview/мерж — вне полномочий структурно (варианта нет).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeStatus {
    InProgress,
    Design,
    ReviewPlan,
    ReviewResult,
    Testing,
    NeedsAttention,
    Hold,
}

impl JudgeStatus {
    /// Ключ ответа decision API (вопрос `next_status`).
    pub fn key(self) -> &'static str {
        match self {
            JudgeStatus::InProgress => "In Progress",
            JudgeStatus::Design => "Design",
            JudgeStatus::ReviewPlan => "Ревью плана",
            JudgeStatus::ReviewResult => "Ревью результата",
            JudgeStatus::Testing => "Тестирование",
            JudgeStatus::NeedsAttention => "Needs attention",
            JudgeStatus::Hold => "hold",
        }
    }

    /// Парсинг ответа модели; неизвестный ключ — None (гард отклонит).
    pub fn parse(key: &str) -> Option<Self> {
        let keys = [
            (JudgeStatus::InProgress, "In Progress"),
            (JudgeStatus::Design, "Design"),
            (JudgeStatus::ReviewPlan, "Ревью плана"),
            (JudgeStatus::ReviewResult, "Ревью результата"),
            (JudgeStatus::Testing, "Тестирование"),
            (JudgeStatus::NeedsAttention, "Needs attention"),
            (JudgeStatus::Hold, "hold"),
        ];
        keys.iter().find(|(_, k)| *k == key).map(|(v, _)| *v)
    }
}

/// Вердикт decision-модели. confidence/owner_needed — без порога применения:
/// страховка — гвард вызовов без прогресса ([`judge_step`]), поля идут в
/// дайджест и лог (решение ревью плана #1364, C4).
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeVerdict {
    pub status: JudgeStatus,
    pub owner_needed: f64,
    pub confidence: f64,
}

/// Шаг гварда судьи: N вызовов подряд без смены статуса строки (слово
/// владельца «порог 3 сверху судьи») — защита от судьи-«hold на всём»
/// (спайковый промах C7 #1364): строка без прогресса попадает владельцу
/// не позже порога.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeStep {
    Keep,
    ForceEscalate,
}

pub fn judge_step(calls_no_progress: u32, escalate_after: u32) -> JudgeStep {
    if escalate_after > 0 && calls_no_progress >= escalate_after {
        JudgeStep::ForceEscalate
    } else {
        JudgeStep::Keep
    }
}

/// Стадия в полномочиях судьи (#1364): сталл- и кап-точки строк на стадиях
/// вне матрицы [`verdict_target`] идут по старой механике — любой вердикт
/// там отклоняется, вызовы API горят без легального ответа. ReviewTesting
/// (#1413) — вердикты тест-ревьювера, не судьи: до отдельного решения
/// стадия остаётся на пингах/капе старой механики.
pub fn judge_stage(current: Status) -> bool {
    matches!(
        current,
        Status::Design
            | Status::InProgress
            | Status::ReviewPlan
            | Status::ReviewResult
            | Status::Testing
    )
}

/// Каноничность перехода (решение ревью плана #1364, C1): `Some(цель)` —
/// переход легален; цель == текущий статус — пробуждение агента без
/// переноса (наследник мягкого пинга, счётчик гварда растёт — статуса
/// нет). `None` — вердикт нелегален (прыжок через стадии канона пайпа):
/// гард отклоняет, hold-эквивалент.
///
/// Матрица канона (перенос = пинг, спавн новой стадии — штатный тик):
/// Design отдаёт план в ревью (не прыгает в работу); работа сдаётся в
/// ревью результата (не отбрасывается в Design — откат в Design делает
/// ревьювер по request_changes); ревью плана возвращает план в Design
/// или паркуется («идёт дальше» — ход ревьювера, не судьи); ревью
/// результата возвращает в работу, сдаёт в тестирование или паркуется;
/// тестирование будится или паркуется (перевод в Review — вердикт
/// тестировщика, не судьи).
pub fn verdict_target(current: Status, verdict: JudgeStatus) -> Option<Status> {
    use JudgeStatus as J;
    use Status as S;
    if verdict == J::Hold {
        return None;
    }
    let target = match verdict {
        J::InProgress => S::InProgress,
        J::Design => S::Design,
        J::ReviewPlan => S::ReviewPlan,
        J::ReviewResult => S::ReviewResult,
        J::Testing => S::Testing,
        J::NeedsAttention => S::NeedsAttention,
        J::Hold => unreachable!("Hold отсечён выше"),
    };
    let allowed: &[S] = match current {
        S::Design => &[S::Design, S::ReviewPlan, S::NeedsAttention],
        S::InProgress => &[S::InProgress, S::ReviewResult, S::NeedsAttention],
        S::ReviewPlan => &[S::ReviewPlan, S::Design, S::NeedsAttention],
        S::ReviewResult => &[
            S::ReviewResult,
            S::InProgress,
            S::Testing,
            S::NeedsAttention,
        ],
        S::Testing => &[S::Testing, S::NeedsAttention],
        // Вне активных стадий судья не вызывается; защита от races —
        // строгий отказ: конвейер не переносит строки вне канона.
        _ => &[],
    };
    allowed.contains(&target).then_some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn judge_status_keys_roundtrip() {
        for key in [
            "In Progress",
            "Design",
            "Ревью плана",
            "Ревью результата",
            "Тестирование",
            "Needs attention",
            "hold",
        ] {
            let parsed = JudgeStatus::parse(key).expect("ключ должен парситься");
            assert_eq!(parsed.key(), key);
        }
        assert_eq!(JudgeStatus::parse("Done"), None);
        assert_eq!(JudgeStatus::parse("User Review"), None);
    }

    #[test]
    fn judge_step_force_escalates_at_threshold() {
        assert_eq!(judge_step(0, 3), JudgeStep::Keep);
        assert_eq!(judge_step(2, 3), JudgeStep::Keep);
        assert_eq!(judge_step(3, 3), JudgeStep::ForceEscalate);
        assert_eq!(judge_step(5, 3), JudgeStep::ForceEscalate);
        // порог 0 = гвард выключен (конфиг-возможность, не дефолт)
        assert_eq!(judge_step(5, 0), JudgeStep::Keep);
    }

    #[test]
    fn verdict_hold_is_never_a_transition() {
        assert_eq!(verdict_target(Status::InProgress, JudgeStatus::Hold), None);
        assert_eq!(verdict_target(Status::Design, JudgeStatus::Hold), None);
    }

    #[test]
    fn verdict_same_status_is_wake_without_transition() {
        assert_eq!(
            verdict_target(Status::InProgress, JudgeStatus::InProgress),
            Some(Status::InProgress)
        );
        assert_eq!(
            verdict_target(Status::Design, JudgeStatus::Design),
            Some(Status::Design)
        );
    }

    #[test]
    fn verdict_canonical_forward_paths_pass() {
        assert_eq!(
            verdict_target(Status::Design, JudgeStatus::ReviewPlan),
            Some(Status::ReviewPlan)
        );
        assert_eq!(
            verdict_target(Status::InProgress, JudgeStatus::ReviewResult),
            Some(Status::ReviewResult)
        );
        assert_eq!(
            verdict_target(Status::ReviewPlan, JudgeStatus::Design),
            Some(Status::Design)
        );
        assert_eq!(
            verdict_target(Status::ReviewResult, JudgeStatus::Testing),
            Some(Status::Testing)
        );
        assert_eq!(
            verdict_target(Status::ReviewResult, JudgeStatus::InProgress),
            Some(Status::InProgress)
        );
    }

    #[test]
    fn verdict_stage_skipping_is_rejected() {
        // прыжок Design → Тестирование мимо ревью плана и работы
        assert_eq!(verdict_target(Status::Design, JudgeStatus::Testing), None);
        // отбрасывание работы в Design — не судейское (это request_changes
        // ревьювера); судья будит агента вместо него
        assert_eq!(
            verdict_target(Status::InProgress, JudgeStatus::Design),
            None
        );
        // Design → работа мимо ревью плана
        assert_eq!(
            verdict_target(Status::Design, JudgeStatus::InProgress),
            None
        );
        // тестирование не раздаёт Review (вердикт тестировщика)
        assert_eq!(
            verdict_target(Status::Testing, JudgeStatus::ReviewResult),
            None
        );
    }

    #[test]
    fn needs_attention_is_always_reachable_from_stages() {
        for current in [
            Status::Design,
            Status::InProgress,
            Status::ReviewPlan,
            Status::ReviewResult,
            Status::Testing,
        ] {
            assert_eq!(
                verdict_target(current, JudgeStatus::NeedsAttention),
                Some(Status::NeedsAttention),
                "паркинг должен быть легален из {current:?}"
            );
        }
    }

    #[test]
    fn verdict_outside_active_stages_is_rejected() {
        // судья не вызывается на неактивных строках; защита от races —
        // строгий отказ любого переноса (кроме… никакого: паркинг тоже нет —
        // строку уже забрал владелец/конвейер другим контуром)
        assert_eq!(
            verdict_target(Status::Done, JudgeStatus::NeedsAttention),
            None
        );
        assert_eq!(verdict_target(Status::Todo, JudgeStatus::InProgress), None);
    }

    #[test]
    fn judge_stage_covers_exactly_the_verdict_matrix() {
        // полномочия судьи = активные стадии матрицы; ReviewTesting (#1413)
        // и неактивные стадии — старая механика
        for status in [
            Status::Design,
            Status::InProgress,
            Status::ReviewPlan,
            Status::ReviewResult,
            Status::Testing,
        ] {
            assert!(judge_stage(status), "{status:?} — стадия судьи");
        }
        for status in [Status::ReviewTesting, Status::Todo, Status::Done] {
            assert!(!judge_stage(status), "{status:?} — вне судьи");
        }
    }
}
