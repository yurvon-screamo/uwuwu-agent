//! Rows whose env was already removed by an explicit script verdict.
//! They must not occupy the batch cap forever.

use std::collections::HashSet;
use std::sync::Mutex;

/// Identity of a gone env. `closed_at` separates a new life of the row
/// after reopen from the previous verdict.
type GoneKey = (String, u64, Option<u64>);

/// Process-local markers: a restart costs one extra batch per board,
/// no state migration and no new port are needed.
#[derive(Debug, Default)]
pub struct EnvGcGone {
    rows: Mutex<HashSet<GoneKey>>,
}

impl EnvGcGone {
    pub fn is_gone(&self, board: &str, number: u64, closed_at: Option<u64>) -> bool {
        self.rows
            .lock()
            .expect("env-gc gone lock")
            .contains(&(board.to_owned(), number, closed_at))
    }

    pub fn mark_gone(&self, board: &str, number: u64, closed_at: Option<u64>) {
        self.rows
            .lock()
            .expect("env-gc gone lock")
            .insert((board.to_owned(), number, closed_at));
    }
}
