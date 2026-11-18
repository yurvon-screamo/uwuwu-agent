//! Боты на вики-носителе (#1150): бот = md-док `Projects/<proj>/Боты/<имя>.md`.
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
pub(crate) struct BotLive {
    inner: std::sync::RwLock<BotLiveView>,
}

#[derive(Clone)]
pub(crate) struct BotLiveView {
    pub owner: Option<String>,
    pub greeting: Option<String>,
    pub manual: String,
}

impl BotLive {
    pub(crate) fn new(owner: Option<String>, greeting: Option<String>, manual: String) -> Self {
        Self {
            inner: std::sync::RwLock::new(BotLiveView {
                owner,
                greeting,
                manual,
            }),
        }
    }

    pub(crate) fn view(&self) -> BotLiveView {
        self.inner.read().expect("bot live lock").clone()
    }

    /// Обновить из свежего дока; true — методичка изменилась (лог).
    pub(crate) fn update(&self, owner: Option<&str>, greeting: Option<&str>, manual: &str) -> bool {
        let mut guard = self.inner.write().expect("bot live lock");
        let changed = guard.manual != manual;
        guard.owner = owner.map(String::from);
        guard.greeting = greeting.map(String::from);
        guard.manual = manual.to_string();
        changed
    }
}
