//! Метки строк, чей env уже снесён явным вердиктом скрипта.
//! Такие строки не должны занимать батч-кап следующих проходов.

use std::collections::HashSet;
use std::sync::Mutex;

/// Ключ исчезнувшего env. `closed_at` отделяет новую жизнь строки
/// после reopen от прежнего вердикта.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GoneKey {
    board: String,
    number: u64,
    closed_at: u64,
}

impl GoneKey {
    fn new(board: &str, number: u64, closed_at: Option<u64>) -> Option<Self> {
        // Кандидатом бывает только строка с датой закрытия
        // (domain/src/task.rs: closed_older_than).
        closed_at.map(|closed_at| Self {
            board: board.to_owned(),
            number,
            closed_at,
        })
    }
}

/// Метки живут в памяти процесса: после рестарта каждый исчезнувший
/// env получит по одному лишнему `ABSENT` и снова выйдет из очереди.
#[derive(Debug, Default)]
pub struct EnvGcGone {
    rows: Mutex<HashSet<GoneKey>>,
}

impl EnvGcGone {
    pub fn is_gone(&self, board: &str, number: u64, closed_at: Option<u64>) -> bool {
        let Some(key) = GoneKey::new(board, number, closed_at) else {
            return false;
        };
        self.rows.lock().expect("env-gc gone lock").contains(&key)
    }

    pub fn mark_gone(&self, board: &str, number: u64, closed_at: Option<u64>) {
        let Some(key) = GoneKey::new(board, number, closed_at) else {
            return;
        };
        self.rows.lock().expect("env-gc gone lock").insert(key);
    }
}
