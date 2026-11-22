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
    /// Без уроков — регресс-гейт #1424: дока `Уроки` нет, поведение как прежде.
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

    /// Обновить из свежего дока; true — методичка изменилась (лог).
    pub(crate) fn update(
        &self,
        owner: Option<&str>,
        greeting: Option<&str>,
        manual: &str,
        lessons: &str,
    ) -> bool {
        let mut guard = self.inner.write().expect("bot live lock");
        let changed = guard.manual != manual;
        guard.owner = owner.map(String::from);
        guard.greeting = greeting.map(String::from);
        guard.manual = manual.to_string();
        guard.lessons = lessons.to_string();
        changed
    }
}
