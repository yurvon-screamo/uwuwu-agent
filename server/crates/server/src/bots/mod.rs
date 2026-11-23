//! Боты на вики-носителе (#1150; раскладка /bot — #1214): бот = md-док `bot/<имя>/README.md`.
//! Раннер — per-bot tokio-задача (TG long-poll → вечная сессия → реплай);
//! супервизор — диф скана вики против запущенных (spawn/stop/restart/re-arm).
//! Контракт вечной сессии: смена model/workspace → пересоздание (память
//! осознанно сбрасывается); token_env/enabled → stop/start, сессия живёт.

mod asset_store;
mod runner;
#[cfg(test)]
mod runner_tests;
mod supervisor;

pub use asset_store::{GitAssetStore, MemoryAssetStore};
pub use supervisor::BotSupervisor;

/// Дифференцированный итог hot-обновления (#1424, ревью Low 2):
/// методичка и уроки меняются разными механизмами доставки — логи
/// раздельные.
#[derive(Clone, Copy, Default)]
pub(crate) struct BotLiveChange {
    pub manual: bool,
    pub lessons: bool,
}

/// Живое (горячее) представление бота: метаданные и методичка читаются
/// раннером с каждого сообщения — правка дока доезжает без рестарта.
/// `manual` — композит методички с уроками (doc-режим, #1424), `lessons` —
/// носитель уроков отдельно (agent-режим, префикс при смене).
pub(crate) struct BotLive {
    inner: std::sync::RwLock<BotLiveView>,
}

#[derive(Clone)]
pub(crate) struct BotLiveView {
    pub owner: Option<String>,
    pub greeting: Option<String>,
    pub manual: String,
    pub lessons: String,
}

impl BotLive {
    /// Без уроков — регресс-гейт #1424: дока `Уроки` нет, поведение как
    /// прежде. Прод-код строит бота через `with_lessons` (уроки читаются
    /// из дока); конструктор без уроков живёт только в тестах —
    /// cfg(test) вместо dead_code в bin-таргете (CI -D warnings,
    /// #1506-ремедиация латентно-красного мастера).
    #[cfg(test)]
    pub(crate) fn new(owner: Option<String>, greeting: Option<String>, manual: String) -> Self {
        Self::with_lessons(owner, greeting, manual, String::new())
    }

    /// Полная форма (#1424): `manual` — композит методички с уроками
    /// (doc-режим), `lessons` — носитель уроков (agent-режим, префикс).
    pub(crate) fn with_lessons(
        owner: Option<String>,
        greeting: Option<String>,
        manual: String,
        lessons: String,
    ) -> Self {
        Self {
            inner: std::sync::RwLock::new(BotLiveView {
                owner,
                greeting,
                manual,
                lessons,
            }),
        }
    }

    pub(crate) fn view(&self) -> BotLiveView {
        self.inner.read().expect("bot live lock").clone()
    }

    /// Обновить из свежего дока; дифференцированный итог — методичка и
    /// уроки доставляются разными механизмами (#1424).
    pub(crate) fn update(
        &self,
        owner: Option<&str>,
        greeting: Option<&str>,
        manual: &str,
        lessons: &str,
    ) -> BotLiveChange {
        let mut guard = self.inner.write().expect("bot live lock");
        let change = BotLiveChange {
            manual: guard.manual != manual,
            lessons: guard.lessons != lessons,
        };
        guard.owner = owner.map(String::from);
        guard.greeting = greeting.map(String::from);
        guard.manual = manual.to_string();
        guard.lessons = lessons.to_string();
        change
    }
}
