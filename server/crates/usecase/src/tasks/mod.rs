//! Задачи: запись (write), чистка (clean, #1158), rename статуса канбана
//! (board_status, #1447) и вьюхи (views).

#[cfg(test)]
mod analys_exit_gate_tests;
pub mod board_status;
pub mod clean;
#[cfg(test)]
mod edit_guard_tests;
mod md_section;
#[cfg(test)]
mod md_section_tests;
pub mod views;
#[cfg(test)]
mod views_tests;
pub mod write;

pub use md_section::body_hash;
