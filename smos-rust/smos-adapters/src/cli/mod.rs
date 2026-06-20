//! `cli` — subcommand runners shared by the unified `smos` binary.
//!
//! Each runner is the body of one subcommand exposed as a callable async
//! function so the single `smos` binary can dispatch to it via clap. The
//! runner does NOT parse CLI args itself — the `smos` binary's clap
//! parser converts `Cli` into the runner-specific `*Args` struct so the
//! runner stays clap-free and the surface stays testable.
//!
//! Layout:
//! - [`tracing_setup`] — install the tracing subscriber (shared by every
//!   subcommand).
//! - [`shutdown`] — Ctrl+C / SIGTERM future (server-only).
//! - [`server_runner`] — `smos serve` (proxy server).
//! - [`finalize_runner`] — `smos finalize` (single-session drain trigger).
//! - [`import_runner`] — `smos import` (opencode transcript importer) +
//!   [`import_helpers`] (pure helpers + unit tests).
//! - [`doctor_runner`] — `smos doctor` (environment validation + report).

pub mod doctor_runner;
pub mod finalize_runner;
pub mod import_helpers;
pub mod import_runner;
pub mod server_runner;
pub mod shutdown;
pub mod tracing_setup;

pub use doctor_runner::{DoctorArgs, run_doctor};
pub use finalize_runner::run_finalize;
pub use import_runner::{ImportArgs, run_import};
pub use server_runner::run_server;
