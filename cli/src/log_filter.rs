//! Builds the `tracing` env-filter for the CLI.
//!
//! Own logs default to `info`, everything else to `warn`. The embedded DB stack
//! (`surrealdb`, `surrealkv`) is force-silenced to `warn` because `surrealdb` prints a
//! block of INFO messages on every kvs-store open — which pollutes CLI output (and breaks
//! agents parsing the output). `RUST_LOG` still overrides the rest. A user can still
//! explicitly target a silenced crate to raise it again, e.g.
//! `RUST_LOG=surrealdb=debug,surrealkv=debug`.

/// Crates that are force-silenced to `warn` unless the user sets them explicitly.
const SILENCED_CRATES: &[&str] = &["surrealkv", "surrealdb"];

/// Default filter applied when `RUST_LOG` is not set: own logs at `info`, rest at `warn`.
pub const DEFAULT_BASE: &str = "uwuwu_cli=info,warn";

/// Compose the final env-filter string from a base (the `RUST_LOG` value, or the default).
///
/// `surrealdb`/`surrealkv` are appended at `warn` unless the base already mentions them
/// (either as a bare target or as `target=level`), so an explicit override wins.
pub fn build(base: &str) -> String {
    let mut parts: Vec<&str> = vec![base];
    let mut extras: Vec<String> = Vec::new();
    for crate_name in SILENCED_CRATES {
        if !is_set(base, crate_name) {
            extras.push(format!("{crate_name}=warn"));
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
            "uwuwu_cli=info,warn,surrealkv=warn,surrealdb=warn"
        );
    }

    #[test]
    fn global_info_silences_db_crate() {
        // The actual user environment: RUST_LOG=INFO globally.
        assert_eq!(build("info"), "info,surrealkv=warn,surrealdb=warn");
    }

    #[test]
    fn explicit_target_level_kept() {
        // Escape hatch: explicit surrealdb=debug must NOT be re-silenced.
        assert_eq!(build("surrealdb=debug"), "surrealdb=debug,surrealkv=warn");
    }

    #[test]
    fn bare_target_kept() {
        // RUST_LOG=surrealdb (no level) → leave surrealdb untouched.
        assert_eq!(build("surrealdb"), "surrealdb,surrealkv=warn");
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
            "uwuwu_cli=debug,surrealdb=trace,surrealkv=warn"
        );
    }

    #[test]
    fn empty_base_is_tolerated() {
        // Empty RUST_LOG must not produce a malformed filter / panic.
        assert_eq!(build(""), ",surrealkv=warn,surrealdb=warn");
    }

    #[test]
    fn no_double_silence_on_repeat() {
        // Idempotent-ish: a crate already mentioned is never duplicated.
        let out = build("surrealkv=warn,surrealdb=warn");
        assert_eq!(out.matches("surrealkv").count(), 1);
        assert_eq!(out.matches("surrealdb").count(), 1);
    }
}
