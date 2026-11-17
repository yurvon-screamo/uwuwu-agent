//! Гард «одна живая сессия на строку» (#1163): решение о спавне по двум
//! источникам — ячейка доски (enrichment) и список сессий исполнителя
//! (union-скан). Класс инцидентов: ночь 15.11 (#52 — невидимые ячейки),
//! #1150 (сирота 429-шторма поверх живой заявки) — спавн поверх живого
//! агента. Принцип #1072 (второй источник живости), перенесённый на
//! серверную эпоху.

use crate::task::Slot;

/// Окно «живого следа» сессии без финала: ход в полёте (outcome пуст)
/// держит спавн, пока след свежий. Верхняя граница страхует зомби-сессии
/// (крэш хода без финала — outcome не проставится никогда): после окна
/// спавн разрешается, повторы каппирует луп-гвард. Окно покрывает
/// провайдер-тишину (инцидент #1150: 72 мин до первого токена).
pub const SLOT_SESSION_LIVE_MS: u64 = 2 * 60 * 60_000;

/// Слепок сессии исполнителя для гарда (минимум из SessionInfo).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSighting {
    pub id: String,
    /// Директория сессии (env-репо строки); пустая — ответ исполнителя
    /// без location, сессия вне раскладки гарда (никогда не держит).
    pub directory: String,
    /// Имя агента; пустое — деф-сессия исполнителя (дев-семья Design/Dev).
    pub agent: String,
    /// Финал хода («succeeded»|…); пустой — ход в полёте или тишина.
    pub outcome: String,
    /// ms эпохи последнего обновления.
    pub updated_ms: u64,
}

/// Вердикт гарда спавна.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnGuard {
    /// Живая сессия той же семьи уже в env строки — спавн держится.
    Hold,
    /// Препятствий нет.
    Allow,
}

/// Ход сессии в полёте: финала нет И след свежий (зомби-страховка).
pub fn sighting_is_live(s: &SessionSighting, now_ms: u64) -> bool {
    s.outcome.is_empty() && now_ms.saturating_sub(s.updated_ms) < SLOT_SESSION_LIVE_MS
}

/// Контракт матчинга директорий: точное равенство не-пустых значений.
/// Пустая директория слепка (исполнитель не отдал location) — вне
/// раскладки, не матчится никогда. Нормализации/симлинки не применяются:
/// обе стороны строят путь из одного env_root конфига; расхождение
/// форматов ловит e2e-фаза стенда («спавн при живой сессии env»).
fn directory_matches(a: &str, b: &str) -> bool {
    !a.is_empty() && a == b
}

/// Семья слота по имени агента: дев-семья (Design/Dev) — деф-сессии без
/// имени; ревью/тест — именованные агенты. Ревьювер, досматривающий
/// вердикт прошлого раунда, спавн нового дева не держит (семьи разные).
pub fn slot_agent_matches(slot: Slot, agent: &str) -> bool {
    agent == slot_agent_name(slot)
}

/// Ожидаемое имя агента слота (пустое — деф-сессия дев-семьи).
pub fn slot_agent_name(slot: Slot) -> &'static str {
    match crate::agent::Agent::of_slot(slot) {
        Some(agent) => match agent {
            crate::agent::Agent::PlanReviewer => "plan-reviewer",
            crate::agent::Agent::CodeQualityReviewer => "code-quality-reviewer",
            crate::agent::Agent::Tester => "tester",
            _ => "",
        },
        None => "",
    }
}

/// Гард спавна слота: держит, если в env-директории строки живёт сессия
/// той же семьи с ходом в полёте. Сироты спавна, клеймы в гонке с тиком
/// и невидимые ячейки не проходят мимо (#1163).
pub fn spawn_guard(
    sightings: &[SessionSighting],
    env_repo: &str,
    slot: Slot,
    now_ms: u64,
) -> SpawnGuard {
    let held = sightings.iter().any(|s| {
        slot_agent_matches(slot, &s.agent)
            && directory_matches(&s.directory, env_repo)
            && sighting_is_live(s, now_ms)
    });
    if held {
        SpawnGuard::Hold
    } else {
        SpawnGuard::Allow
    }
}

/// Сессия числится за исполнителем — второй источник против лживой
/// Dead-пробы (#1072): сессия в списке, а проба говорит Dead, значит
/// солгала; респавн отменяется, вердикт уйдёт wakeup-путём.
pub fn sighting_known(sightings: &[SessionSighting], session_id: &str) -> bool {
    sightings.iter().any(|s| s.id == session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sighting(
        id: &str,
        directory: &str,
        agent: &str,
        outcome: &str,
        updated_ms: u64,
    ) -> SessionSighting {
        SessionSighting {
            id: id.into(),
            directory: directory.into(),
            agent: agent.into(),
            outcome: outcome.into(),
            updated_ms,
        }
    }

    const ENV: &str = "/env/1/repo";
    const NOW: u64 = 10_000_000;

    #[test]
    fn live_is_empty_outcome_with_fresh_trace() {
        let fresh = sighting("s1", ENV, "", "", NOW - 60_000);
        assert!(sighting_is_live(&fresh, NOW), "ход в полёте — живая");
        let zombie = sighting("s2", ENV, "", "", NOW - SLOT_SESSION_LIVE_MS - 1);
        assert!(
            !sighting_is_live(&zombie, NOW),
            "тишина за окном следа — зомби-страховка разрешает спавн"
        );
    }

    #[test]
    fn finished_outcome_is_not_live_even_fresh() {
        let done = sighting("s1", ENV, "", "succeeded", NOW - 1_000);
        assert!(
            !sighting_is_live(&done, NOW),
            "финал хода — спавн нового раунда не держится"
        );
    }

    #[test]
    fn hold_on_live_dev_family_in_env() {
        let orphan = sighting("s1", ENV, "", "", NOW - 60_000);
        assert_eq!(
            spawn_guard(&[orphan], ENV, Slot::Dev, NOW),
            SpawnGuard::Hold,
            "сирота дев-семьи в env держит спавн (#1150)"
        );
    }

    #[test]
    fn reviewer_session_does_not_hold_dev_spawn() {
        let reviewer = sighting("s1", ENV, "code-quality-reviewer", "", NOW - 60_000);
        assert_eq!(
            spawn_guard(&[reviewer], ENV, Slot::Dev, NOW),
            SpawnGuard::Allow,
            "чужая семья (ревьювер за вердиктом) не держит дев-спавн"
        );
    }

    #[test]
    fn named_slot_held_by_same_agent_only() {
        let reviewer = sighting("s1", ENV, "code-quality-reviewer", "", NOW - 60_000);
        assert_eq!(
            spawn_guard(&[reviewer.clone()], ENV, Slot::ResultReview, NOW),
            SpawnGuard::Hold,
            "живой ревьювер держит спавн своего слота"
        );
        assert_eq!(
            spawn_guard(&[reviewer], ENV, Slot::Test, NOW),
            SpawnGuard::Allow,
            "чужой слот не держится"
        );
    }

    #[test]
    fn other_directory_and_blank_directory_never_hold() {
        let other = sighting("s1", "/env/2/repo", "", "", NOW - 60_000);
        let blank = sighting("s2", "", "", "", NOW - 60_000);
        assert_eq!(
            spawn_guard(&[other, blank], ENV, Slot::Dev, NOW),
            SpawnGuard::Allow,
            "чужая строка и слепок без location не держат (ревью Common-1)"
        );
    }

    #[test]
    fn known_by_id_even_with_finished_outcome() {
        let done = sighting("ses_x", ENV, "", "succeeded", NOW - 3_600_000);
        assert!(
            sighting_known(&[done], "ses_x"),
            "сессия в списке исполнителя — Dead-проба солгала, irrespective финала"
        );
    }

    #[test]
    fn slot_agent_names_bind_families() {
        assert_eq!(slot_agent_name(Slot::Dev), "");
        assert_eq!(slot_agent_name(Slot::Design), "");
        assert_eq!(slot_agent_name(Slot::PlanReview), "plan-reviewer");
        assert_eq!(slot_agent_name(Slot::ResultReview), "code-quality-reviewer");
        assert_eq!(slot_agent_name(Slot::Test), "tester");
        assert!(slot_agent_matches(Slot::Dev, ""));
        assert!(!slot_agent_matches(Slot::Dev, "developer"));
    }
}
