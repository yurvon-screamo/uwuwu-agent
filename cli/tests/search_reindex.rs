use std::fs;
use std::path::Path;
use std::time::Duration;

use tempfile::tempdir;

use uwuwu_cli::cache::Cache;
use uwuwu_cli::search::reindex_file;

const SAMPLE_DOC: &str = "---\ntitle: Test\ndescription: Sample\nupdated: 2026-07-14\n---\n\n# Test\n\n## Setup\n\nFirst section body.\n\n## Gotchas\n\nSecond section body.\n";

async fn ollama_available() -> bool {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    client
        .get("http://localhost:11434/api/tags")
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn ensure_experience(root: &Path) {
    fs::create_dir_all(root.join("experience/databases")).unwrap();
}

// surrealdb 3.x closes the embedded store asynchronously: Surreal's Drop sends a
// channel message to a background task instead of closing synchronously. A second
// open in the same process (reindex-then-verify here) can briefly hit the OS file
// lock. Retry with backoff until the background close finishes. Production is
// unaffected — each CLI run is its own process, so the OS reclaims all handles on
// exit.
//
// Matcher is intentionally narrow: `os error 33` is Windows LOCK_VIOLATION, stable
// across locales inside the io-error Display string. A loose `contains("lock")`
// would wrongly match "block"/"deadlock" and mask unrelated failures. A typed
// `raw_os_error()==Some(33)` check would need the io::Error preserved in the chain,
// but cache.rs string-wraps the error via `anyhow::anyhow!`.
fn is_lock_violation(err: &anyhow::Error) -> bool {
    err.to_string().contains("os error 33")
}

async fn reopen_cache(path: &Path) -> Cache {
    const ATTEMPTS: u8 = 30;
    const BACKOFF: Duration = Duration::from_millis(100);

    let mut last_err: Option<anyhow::Error> = None;
    for _ in 0..ATTEMPTS {
        match Cache::open(path).await {
            Ok(cache) => return cache,
            Err(e) if is_lock_violation(&e) => {
                last_err = Some(e);
                tokio::time::sleep(BACKOFF).await;
            }
            Err(e) => panic!("cache reopen failed (non-lock): {e}"),
        }
    }
    panic!("cache reopen failed after {ATTEMPTS} attempts (lock never released): {last_err:?}");
}

#[tokio::test]
async fn reindex_populates_cache_with_chunks_for_file() {
    if !ollama_available().await {
        eprintln!("skipping: ollama not available on localhost:11434");
        return;
    }

    let root = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    ensure_experience(root.path());
    fs::write(root.path().join("experience/databases/foo.md"), SAMPLE_DOC).unwrap();
    let cache_path = cache_dir.path().join("embeddings.db");

    reindex_file("databases/foo.md", root.path(), &cache_path)
        .await
        .expect("reindex should succeed");

    let cache = reopen_cache(&cache_path).await;
    let loaded = cache.load_all().await.expect("cache loads");

    let key = "experience/databases/foo.md".to_string();
    let entry = loaded
        .get(&key)
        .unwrap_or_else(|| panic!("cache should contain {key}"));
    assert!(
        !entry.1.is_empty(),
        "chunks vector must be non-empty after reindex"
    );
}
