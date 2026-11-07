//! Контракт ID ядра SiYuan: параметры notebook/doc kernel-API принимают
//! только ИД узла вида `20261103224312-fbrf5fp` (14 цифр + '-' + хвост).
//! С именем («Projects») или hpath ядро отвечает «invalid ID argument»,
//! что без валидации маскируется под «док не найден» — так сломался
//! `read` при миграции на SiYuan (2026-11-04).

use anyhow::{bail, Result};

/// Сгенерировать валидный ИД узла (формат ядра: 14 цифр таймстампа + '-' +
/// 7 символов [a-z0-9]): клиент обязан сам генерировать keyID при создании
/// колонок database (/api/av/addAttributeViewKey). Случайный хвост —
/// наносекунды таймстампа: уникальность в пределах секунды достаточна.
pub fn new_node_id() -> String {
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let stamp = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
    let mut nanos =
        chrono::Local::now().timestamp_subsec_nanos() as u64 ^ (std::process::id() as u64) << 32;
    let mut tail = String::with_capacity(7);
    for _ in 0..7 {
        tail.push(ALPHABET[(nanos % ALPHABET.len() as u64) as usize] as char);
        nanos /= ALPHABET.len() as u64;
    }
    format!("{stamp}-{tail}")
}

/// Проверить, что строка — ИД узла ядра, иначе внятная ошибка с именем параметра.
pub fn ensure_node_id(value: &str, param: &str) -> Result<()> {
    let bytes = value.as_bytes();
    let is_node_id = bytes.len() > 15
        && bytes[..14].iter().all(|b| b.is_ascii_digit())
        && bytes[14] == b'-'
        && bytes[15..].iter().all(|b| b.is_ascii_alphanumeric());
    if is_node_id {
        Ok(())
    } else {
        bail!("{param} должен быть ИД ядра (14 цифр + '-' + хвост), получено «{value}»")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_node_id_passes() {
        ensure_node_id("20261103224312-fbrf5fp", "notebook_id").unwrap();
    }

    #[test]
    fn notebook_name_is_rejected() {
        assert!(ensure_node_id("Projects", "notebook_id").is_err());
    }

    #[test]
    fn hpath_is_rejected() {
        assert!(ensure_node_id("/uwuwu/README", "notebook_id").is_err());
    }

    #[test]
    fn id_with_sql_injection_tail_is_rejected() {
        assert!(ensure_node_id("20261103224312-fbrf5' OR 1=1--", "doc_id").is_err());
    }

    #[test]
    fn multibyte_string_is_rejected_without_panic() {
        assert!(ensure_node_id("ноутбук Проекты", "notebook_id").is_err());
    }

    #[test]
    fn generated_node_id_passes_kernel_contract() {
        let id = new_node_id();
        ensure_node_id(&id, "key_id").unwrap();
    }

    #[test]
    fn generated_node_ids_are_distinct() {
        let a = new_node_id();
        let b = new_node_id();
        assert_ne!(a, b, "последовательные id различаются");
    }
}
