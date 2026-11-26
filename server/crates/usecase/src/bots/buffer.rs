//! Буфер ввода бота (#1596): чистая логика приёма апдейтов и порядка
//! батча. Материал (форвард/медиа) идёт в промпте раньше команд, внутри
//! каждой группы — по монотонному `update_id` Telegram.

use crate::bots::ports::TgMessage;
use std::cmp::Reverse;

/// Один элемент ожидающего ввода. `update_id` — ключ порядка и offset;
/// `material` сохраняется при правке, чтобы замена текста не сбросила
/// приоритет исходного материала.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingItem {
    pub update_id: u64,
    pub message_id: i64,
    pub message: TgMessage,
    pub material: bool,
    /// Правка уже сброшенного сообщения — отдельный элемент с меткой.
    pub is_edit: bool,
}

#[derive(Debug, Default)]
pub struct InputBuffer {
    items: Vec<PendingItem>,
    max_update_id: u64,
}

impl InputBuffer {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn max_update_id(&self) -> u64 {
        self.max_update_id
    }

    /// Приём сообщения или правки. Правка находит оригинал по `message_id`
    /// и заменяет его текст, сохраняя позицию и приоритет материала.
    pub fn push(&mut self, update_id: u64, message: TgMessage, is_edit: bool) {
        self.max_update_id = self.max_update_id.max(update_id);
        let material = is_material(&message);
        if message.message_id != 0 {
            if let Some(pos) = self
                .items
                .iter()
                .position(|item| item.message_id == message.message_id)
            {
                self.items[pos].message = message;
                self.items[pos].material |= material;
                return;
            }
        }
        self.items.push(PendingItem {
            update_id,
            message_id: message.message_id,
            message,
            material,
            is_edit,
        });
    }

    /// Упорядоченный вид для сброса: материал первым, внутри каждой
    /// группы — по update_id. Порядок вставки не меняется.
    pub fn ordered(&self) -> Vec<&PendingItem> {
        let mut items: Vec<&PendingItem> = self.items.iter().collect();
        items.sort_by_key(|item| (Reverse(item.material), item.update_id));
        items
    }

    /// Сброс: вернуть элементы в порядке батча и обнулить накопленный offset.
    pub fn take(&mut self) -> Vec<PendingItem> {
        self.max_update_id = 0;
        let mut items = std::mem::take(&mut self.items);
        items.sort_by_key(|item| (Reverse(item.material), item.update_id));
        items
    }
}

/// Носитель материала: пересланный пост или вложение (#1416, #1171).
pub fn is_material(message: &TgMessage) -> bool {
    message.forward.is_some() || message.media.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bots::ports::{MediaKind, TgMedia};

    fn msg(message_id: i64, text: &str, forward: Option<&str>) -> TgMessage {
        TgMessage {
            message_id,
            chat_type: "private".into(),
            chat_id: 42,
            from_username: Some("yurvon".into()),
            text: Some(text.into()),
            caption: None,
            media: None,
            forward: forward.map(String::from),
        }
    }

    fn photo(message_id: i64) -> TgMessage {
        TgMessage {
            media: Some(TgMedia {
                kind: MediaKind::Photo,
                file_id: "asset".into(),
                file_name: None,
                file_size: Some(10),
                mime: None,
            }),
            ..msg(message_id, "", None)
        }
    }

    #[test]
    fn late_material_precedes_following_commands() {
        let mut buf = InputBuffer::default();
        buf.push(9, msg(1, "первое", None), false);
        buf.push(10, msg(2, "второе", None), false);
        buf.push(11, msg(3, "третье", None), false);
        buf.push(12, msg(4, "пост", Some("канал @x")), false);
        let items = buf.take();
        let texts: Vec<&str> = items
            .iter()
            .map(|item| item.message.text.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(texts, vec!["пост", "первое", "второе", "третье"]);
    }

    #[test]
    fn material_group_keeps_update_id_order() {
        let mut buf = InputBuffer::default();
        buf.push(3, photo(31), false);
        buf.push(4, msg(32, "команда", None), false);
        buf.push(5, msg(33, "пост", Some("канал @x")), false);
        let ids: Vec<u64> = buf.take().iter().map(|item| item.update_id).collect();
        assert_eq!(ids, vec![3, 5, 4]);
    }

    #[test]
    fn edit_replaces_original_in_place_and_keeps_material() {
        let mut buf = InputBuffer::default();
        buf.push(7, msg(70, "было", Some("канал @x")), false);
        buf.push(8, msg(70, "стало", None), true);
        let items = buf.take();
        assert_eq!(items.len(), 1, "правка не дублирует оригинал");
        assert_eq!(items[0].message.text.as_deref(), Some("стало"));
        assert!(items[0].material, "материал после правки сохранён");
        assert!(!items[0].is_edit, "позиция оригинала сохранена");
    }

    #[test]
    fn edit_of_flushed_message_is_separate_item() {
        let mut buf = InputBuffer::default();
        buf.push(9, msg(90, "новое", None), true);
        let items = buf.take();
        assert_eq!(items.len(), 1);
        assert!(items[0].is_edit);
        assert_eq!(buf.max_update_id(), 0, "offset сброшен вместе с батчем");
    }
}
