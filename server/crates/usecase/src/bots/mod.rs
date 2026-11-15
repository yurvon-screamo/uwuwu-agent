//! Боты на вики-носителе (#1150): бот = md-док `Projects/<proj>/Боты/<имя>.md`
//! (fm: title/enabled/model/token_env/owner/workspace/greeting; тело —
//! методичка). Супервизор сервера сканирует папки, диф держит per-bot
//! tokio-задачи с TG long-poll. Здесь: парс спеки, диф-действия,
//! классификация апдейтов, сборка промпта; порты ТГ и bot-состояния.

mod flow;
mod ports;
mod spec;

pub use flow::{build_prompt, chunk_reply, classify_message, plan_action};
pub use ports::{BotStateStore, TelegramGateway, TgMessage, TgUpdate};
pub use spec::parse_bot_doc;
