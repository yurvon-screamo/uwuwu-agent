//! Build-time metadata inlined into the binary via rustc-env.
//!
//! `UWUWU_GIT_HASH` — `git describe --always --dirty` from the crate dir;
//! fallback `unknown` outside a git context (sandboxes, tarballs), so the
//! build never fails there. `UWUWU_BUILD_DATE` — UTC date of the build,
//! pure calendar math: no build-deps, no external `date` (Windows-safe).

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rustc-env=UWUWU_GIT_HASH={}", git_hash());
    println!("cargo:rustc-env=UWUWU_BUILD_DATE={}", build_date_utc());
    emit_git_rerun_triggers();
}

fn git_hash() -> String {
    Command::new("git")
        .args(["describe", "--always", "--dirty"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|hash| !hash.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn build_date_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

/// Howard Hinnant's civil_from_days: days since 1970-01-01 → (year, month, day).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = era * 400 + yoe + i64::from(month <= 2);
    (year, month, day)
}

fn emit_git_rerun_triggers() {
    // The crate dir is cli/, the repo root two levels up.
    let git_meta = Path::new("../../.git");
    if git_meta.is_dir() {
        println!("cargo:rerun-if-changed=../../.git/HEAD");
        println!("cargo:rerun-if-changed=../../.git/refs/heads");
    } else if git_meta.is_file() {
        // Worktree: `.git` is a gitdir pointer file whose contents do not
        // change on new commits — incremental rebuilds may keep a stale
        // hash (`touch cli/build.rs` forces a refresh).
        println!("cargo:rerun-if-changed=../../.git");
    }
}
