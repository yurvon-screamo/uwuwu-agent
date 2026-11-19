//! Боты на вики-носителе (#1150; раскладка /bot — #1214): бот = md-док `bot/<имя>/README.md`
//! (fm: title/enabled/model/token_env/owner/workspace/greeting; тело —
//! методичка). Супервизор сервера сканирует папки, диф держит per-bot
//! tokio-задачи с TG long-poll. Здесь: парс спеки, диф-действия,
//! классификация апдейтов, сборка промпта; порты ТГ и bot-состояния.

mod assets;
mod flow;
mod ports;
mod spec;

pub use assets::normalize_asset_name;
pub use flow::{build_prompt, chunk_reply, classify_message, plan_action};
pub use ports::{
    BotAssetStore, BotStateStore, Incoming, MediaKind, TelegramGateway, TgMedia, TgMessage,
    TgUpdate,
};
pub use spec::parse_bot_doc;
