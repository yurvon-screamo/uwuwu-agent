//! IO checks for the doctor subcommand.
//!
//! Each submodule owns one external system:
//! - [`binaries`] — smos binary presence + version
//! - [`ollama`] — Ollama connectivity + required models + reranker
//! - [`surreal`] — SurrealDB connect + migrations + stats snapshot
//!
//! All checks return [`CheckResult`] rows; the orchestrator in [`mod`]
//! concatenates them into a [`DoctorReport`]. Tests live in
//! `tests/doctor_unit.rs` and cover only the pure helpers (matching,
//! formatting, aggregation); IO paths are exercised manually during the
//! smoke test.

pub mod binaries;
pub mod ollama;
pub mod surreal;

use std::time::Duration;

use super::types::{CheckResult, DoctorReport};
use crate::config::SmosConfig;

/// Orchestrator flags parsed from the CLI. Mirrors the `smos doctor`
/// subcommand args 1:1 so the orchestrator does not depend on `clap`.
#[derive(Debug, Clone, Default)]
pub struct DoctorFlags {
    pub skip_ollama: bool,
}

/// Try to build a reqwest client without panicking. Returns `None` if the
/// builder rejects the configured TLS stack (rustls init failure, etc).
/// The doctor turns a `None` into WARN rows for the Ollama + reranker
/// checks instead of crashing — a TLS init failure is rare but must NOT
/// abort a diagnostic tool whose entire purpose is to report degraded
/// infrastructure.
fn try_build_http_client() -> Option<reqwest::Client> {
    reqwest::Client::builder().build().ok()
}

/// Build the two check-result rows emitted when the HTTP client itself
/// could not be constructed. Both probes share the same root cause, so the
/// recommendation is identical.
fn http_client_unavailable_rows() -> Vec<CheckResult> {
    vec![
        CheckResult::warn(
            "Ollama connectivity",
            "HTTP client construction failed (TLS init error)",
        )
        .with_recommendation("verify rustls/native-tls setup and re-run"),
        CheckResult::warn(
            "Reranker",
            "HTTP client construction failed (TLS init error)",
        )
        .with_recommendation("reranker optional; resolve TLS setup and re-run"),
    ]
}

/// Run the full check matrix against the config and return a populated
/// [`DoctorReport`]. Order is fixed so the operator reads top-to-bottom:
/// binaries first (cheapest), then external services, then stats.
///
/// `config_path` is propagated verbatim into the report header so an
/// operator inspecting a saved Markdown artefact can tell which config was
/// actually validated (the `--config` flag overrides it at the CLI).
pub async fn run_full_check(
    config: &SmosConfig,
    flags: &DoctorFlags,
    config_path: &str,
) -> DoctorReport {
    let mut report = DoctorReport::new(now_iso(), config_path);

    let binary_results = binaries::check_binaries().await;
    report.extend(binary_results);

    if !flags.skip_ollama {
        match try_build_http_client() {
            Some(client) => {
                let ollama_timeout = Duration::from_secs(config.ollama.timeout_seconds);
                let ollama_results =
                    ollama::check_ollama(&client, &config.ollama, ollama_timeout).await;
                report.extend(ollama_results);

                let reranker_timeout = Duration::from_secs(config.reranker.timeout_seconds);
                let reranker_result =
                    ollama::check_reranker(&client, &config.reranker, reranker_timeout).await;
                report.push(reranker_result);
            }
            None => report.extend(http_client_unavailable_rows()),
        }
    }

    let surreal_results = surreal::check_surreal(&config.surreal, &config.session).await;
    let stats = match surreal_results {
        Ok((rows, snapshot)) => {
            report.extend(rows);
            snapshot
        }
        Err(rows) => {
            report.extend(rows);
            None
        }
    };
    report.stats = stats;

    report
}

/// Run only the SurrealDB stats check. Fast (<1 s) — used by `--stats`.
pub async fn run_stats_only(config: &SmosConfig, config_path: &str) -> DoctorReport {
    let mut report = DoctorReport::new(now_iso(), config_path);
    match surreal::check_surreal(&config.surreal, &config.session).await {
        Ok((rows, snapshot)) => {
            report.extend(rows);
            report.stats = snapshot;
        }
        Err(rows) => report.extend(rows),
    }
    report
}

/// RFC 3339 UTC timestamp for the report header.
fn now_iso() -> String {
    use time::format_description::well_known::Rfc3339;
    let now = time::OffsetDateTime::now_utc();
    now.format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::CheckStatus;

    #[test]
    fn default_flags_match_smoke_test_spec() {
        let f = DoctorFlags::default();
        assert!(!f.skip_ollama);
    }

    #[test]
    fn now_iso_is_parseable_rfc3339() {
        let ts = now_iso();
        let parsed =
            time::OffsetDateTime::parse(&ts, &time::format_description::well_known::Rfc3339);
        assert!(parsed.is_ok(), "doctor timestamp must be RFC 3339: {ts}");
    }

    #[test]
    fn try_build_http_client_returns_some_in_default_rustls_setup() {
        // The doctor crate uses `reqwest = { features = ["rustls-tls"] }`,
        // so a fresh `Client::builder().build()` succeeds in any sane host
        // environment. A `None` here would mean rustls itself is broken.
        assert!(
            try_build_http_client().is_some(),
            "default rustls client must construct in the test environment"
        );
    }

    #[test]
    fn http_client_unavailable_rows_emit_two_warn_results_with_hints() {
        let rows = http_client_unavailable_rows();
        assert_eq!(rows.len(), 2, "both Ollama and reranker probes are blocked");
        assert_eq!(rows[0].name, "Ollama connectivity");
        assert_eq!(rows[0].status, CheckStatus::Warn);
        assert!(
            rows[0]
                .recommendation
                .as_deref()
                .unwrap()
                .contains("rustls"),
            "Ollama hint must point at TLS setup"
        );
        assert_eq!(rows[1].name, "Reranker");
        assert_eq!(rows[1].status, CheckStatus::Warn);
        assert!(
            rows[1]
                .recommendation
                .as_deref()
                .unwrap()
                .contains("optional"),
            "reranker is optional, hint must say so"
        );
    }
}
