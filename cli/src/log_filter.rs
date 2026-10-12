//! Builds the `tracing` env-filter for the CLI.
//!
//! Own logs default to `info`, everything else to `warn`. The embedded DB stack
//! (`surrealdb`, `surrealkv`) is force-silenced to `error`: `surrealdb` prints a
//! block of INFO messages on every kvs-store open, and `surrealkv` WARNs
//! "No runtime available for closing the store correctly" at exit — its Tree is
//! dropped from a global registry after the tokio runtime is gone, so the WARN
//! is structural and cannot be fixed on our side. `RUST_LOG` still overrides
//! the rest; an explicit per-crate target re-enables it for debugging.

/// Crates that are force-silenced to `error` unless the user sets them explicitly.
const SILENCED_CRATES: &[&str] = &["surrealkv", "surrealdb"];

/// Default filter applied when `RUST_LOG` is not set: own logs at `info`, rest at `warn`.
pub const DEFAULT_BASE: &str = "uwuwu_cli=info,warn";

/// Compose the final env-filter string from a base (the `RUST_LOG` value, or the default).
///
/// `surrealdb`/`surrealkv` are appended at `error` unless the base already mentions them
/// (either as a bare target or as `target=level`), so an explicit override wins.
pub fn build(base: &str) -> String {
    let mut parts: Vec<&str> = vec![base];
    let mut extras: Vec<String> = Vec::new();
    for crate_name in SILENCED_CRATES {
        if !is_set(base, crate_name) {
            extras.push(format!("{crate_name}=error"));
        }
    }
    if extras.is_empty() {
        return base.to_string();
    }
    parts.extend(extras.iter().map(String::as_str));
    parts.join(",")
}

/// True if `base` already references `crate_name` as a directive (`crate` or `crate=...`).
fn is_set(base: &str, crate_name: &str) -> bool {
    base.split(',').any(|directive| {
        let directive = directive.trim();
        directive == crate_name
            || directive
                .strip_prefix(crate_name)
                .is_some_and(|rest| rest.starts_with('='))
    })
}

#[cfg(test)]
mod tests {
    use super::build;

    #[test]
    fn default_when_unset() {
        // No RUST_LOG → own logs at info, rest at warn, both DB crates silenced.
        assert_eq!(
            build("uwuwu_cli=info,warn"),
            "uwuwu_cli=info,warn,surrealkv=error,surrealdb=error"
        );
    }

    #[test]
    fn global_info_silences_db_crate() {
        // The actual user environment: RUST_LOG=INFO globally.
        assert_eq!(build("info"), "info,surrealkv=error,surrealdb=error");
    }

    #[test]
    fn explicit_target_level_kept() {
        // Escape hatch: explicit surrealdb=debug must NOT be re-silenced.
        assert_eq!(build("surrealdb=debug"), "surrealdb=debug,surrealkv=error");
    }

    #[test]
    fn bare_target_kept() {
        // RUST_LOG=surrealdb (no level) → leave surrealdb untouched.
        assert_eq!(build("surrealdb"), "surrealdb,surrealkv=error");
    }

    #[test]
    fn both_targets_set_adds_nothing() {
        assert_eq!(
            build("surrealkv=off, surrealdb=off"),
            "surrealkv=off, surrealdb=off"
        );
    }

    #[test]
    fn mixed_directives() {
        assert_eq!(
            build("uwuwu_cli=debug,surrealdb=trace"),
            "uwuwu_cli=debug,surrealdb=trace,surrealkv=error"
        );
    }

    #[test]
    fn empty_base_is_tolerated() {
        // Empty RUST_LOG must not produce a malformed filter / panic.
        assert_eq!(build(""), ",surrealkv=error,surrealdb=error");
    }

    #[test]
    fn no_double_silence_on_repeat() {
        // Idempotent-ish: a crate already mentioned is never duplicated.
        let out = build("surrealkv=error,surrealdb=error");
        assert_eq!(out.matches("surrealkv").count(), 1);
        assert_eq!(out.matches("surrealdb").count(), 1);
    }
}
