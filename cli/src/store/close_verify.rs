//! Verify-after-write гарда close (задача 1127): GitHub подтверждал PATCH
//! state=closed ответом success, не применив переход (события closed в
//! timeline нет). Сверка контрольного GET с ожиданием — единственный
//! механический гард; read-after-write в моменте может врать вместе с
//! PATCH-ответом.

use std::time::Duration;

/// Пауза между PATCH и контрольным GET (и между ретраями): GET/list GitHub
/// имеет задокументированный лаг консистентности 2–4s (см. LIST_LAG
/// github_store_e2e) — пауза с запасом, чтобы сверка не читала stale-open
/// и не жгла ретраи впустую.
pub(super) const VERIFY_PAUSE: Duration = Duration::from_secs(5);

/// PATCH идемпотентен: первый PATCH + до двух добивающих ретраев.
pub(super) const RETRY_ATTEMPTS: usize = 2;

/// Осознанное допущение: гард не различает «PATCH потерян» и «issue
/// переоткрыли в окне ретрая» — во втором случае ретрай закроет заново.
/// Окно секунды, платформа одно-владельческая, вероятность низкая; спорный
/// исход частично покрывает громкий отказ при исчерпании ретраев.
///
/// Отказ называет применённую часть вызывающему коду: текст использует
/// close_impl, который знает про приземлённый note-коммент.
pub(super) fn state_verdict(fresh_state: &str) -> Result<(), String> {
    if fresh_state == "closed" {
        return Ok(());
    }
    Err(format!(
        "issue остался «{fresh_state}» после PATCH state=closed — GitHub false-success \
         (см. задачу 1127): закрой вручную или повтори task close (PATCH идемпотентен)"
    ))
}

#[cfg(test)]
mod tests {
    use super::state_verdict;

    #[test]
    fn verdict_accepts_confirmed_close() {
        assert!(state_verdict("closed").is_ok());
    }

    #[test]
    fn verdict_rejects_stale_open_with_task_pin() {
        let err = state_verdict("open").unwrap_err();
        assert!(err.contains("false-success"), "нет пина инцидента: {err}");
        assert!(err.contains("1127"), "нет ссылки на задачу: {err}");
        assert!(err.contains("open"), "нет фактического состояния: {err}");
    }

    #[test]
    fn verdict_rejects_any_non_closed_state() {
        // GET state принимает только open/closed, но вердикт белый —
        // любой не-closed это отказ, а не тихий успех.
        assert!(state_verdict("").is_err());
    }
}
