//! SMOS proxy configuration (`smos.toml`).
//!
//! The config is layered: sections present in the TOML file override the
//! [`Default`] values; any section missing from the file falls back to its
//! canonical default. This keeps the in-repo `smos.toml` minimal (operators
//! override only what they need) while `cargo run --bin smos -- serve` still
//! works with no file at all.
//!
//! The external `config` crate is referenced as `::config` because this module
//! is itself named `config` — the leading `::` unambiguously reaches the
//! external crate instead of recursing into `crate::config`.

use serde::{Deserialize, Serialize};
pub use smos_domain::config::{
    ConfidenceConfig, ExtractionConfig, HeatConfig, MergeConfig, NliConfig, RetrievalConfig,
};

/// Root configuration.
///
/// Sections that originate in `smos-domain` (`retrieval`, `merge`,
/// `confidence`, `heat`, `nli`) are re-exported from this module so callers
/// have a single import path. Sections that only make sense at the adapter
/// boundary (`surreal`, `server`, `upstream`, `ollama`, `reranker`,
/// `nli_backend`, `session`) live here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SmosConfig {
    #[serde(default)]
    pub surreal: SurrealConfig,

    #[serde(default)]
    pub server: ServerConfig,

    #[serde(default)]
    pub upstream: UpstreamConfig,

    #[serde(default)]
    pub ollama: OllamaConfig,

    #[serde(default)]
    pub reranker: RerankerConfig,

    #[serde(default)]
    pub retrieval: RetrievalConfig,

    #[serde(default)]
    pub merge: MergeConfig,

    #[serde(default)]
    pub confidence: ConfidenceConfig,

    #[serde(default)]
    pub heat: HeatConfig,

    /// Pure-domain NLI verdict thresholds. Backs
    /// [`NliResult::is_contradiction`] / [`NliResult::is_entailment`] at the
    /// use-case level. Separate from [`NliBackendConfig`] which carries the
    /// *process* configuration (model id, cache dir) — that belongs at the
    /// adapter boundary because the domain layer is IO-free.
    #[serde(default)]
    pub nli: NliConfig,

    /// Semantic dedup safety net for fact extraction (`persist_facts` step 2).
    /// Backs the cosine-similarity gate the extractor falls back to when
    /// `FactId = SHA1(content)` exact match misses a rephrased re-observation.
    #[serde(default)]
    pub extraction: ExtractionConfig,

    /// Native ort + ONNX Runtime backend configuration. Owns the Hugging Face
    /// model id + cache directory — never read by the domain layer.
    #[serde(default)]
    pub nli_backend: NliBackendConfig,

    #[serde(default)]
    pub session: SessionConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SurrealConfig {
    pub path: String,
    pub namespace: String,
    pub database: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub shutdown_extraction_grace_seconds: u64,
    pub enable_response_extraction: bool,
    pub graceful_degradation: bool,
    pub log_format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UpstreamConfig {
    pub url: String,
    pub api_key: String,
    pub auth_header: String,
    pub timeout_seconds: u64,
}

/// Ollama server connection + model selection.
///
/// `extraction_seed` + `extraction_temperature` make the extractor
/// **near-deterministic** (best-effort, not a hard guarantee): pairing
/// `temperature = 0.0` (greedy decoding) with a pinned `seed` makes
/// `/api/chat` re-yield the same bullet list across runs on the same backend.
/// Note: backend (llama.cpp), device (CPU vs GPU), and quantization can still
/// introduce tiny variations — which is exactly why the semantic dedup safety
/// net (`ExtractionConfig.dedup_cosine_threshold`) is the second, mandatory
/// layer rather than an optional add-on.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OllamaConfig {
    /// Base URL of the Ollama server (no path suffix).
    pub url: String,
    /// Model id passed in the `model` field of `/api/embeddings`.
    pub embedding_model: String,
    /// Model id used by the extractor when calling `/api/chat`.
    pub extraction_model: String,
    /// Per-request HTTP timeout.
    pub timeout_seconds: u64,
    /// Sampling seed passed to Ollama `options.seed`. Default 42 mirrors the
    /// value validated in the reproduction test (3 runs → identical output).
    pub extraction_seed: u32,
    /// Sampling temperature passed to Ollama `options.temperature`. `0.0`
    /// (greedy decoding) is the near-deterministic baseline.
    pub extraction_temperature: f32,
}

/// llama.cpp reranker server connection.
///
/// The adapter expects an OpenAI-compatible `/v1/rerank` endpoint (e.g. the
/// `llama-server` binary shipped with llama.cpp when started with a reranker
/// model such as Qwen3-Reranker).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RerankerConfig {
    /// Base URL of the reranker server (no path suffix).
    pub url: String,
    /// Model id passed in the `model` field of `/v1/rerank`.
    pub model: String,
    /// Per-request HTTP timeout.
    pub timeout_seconds: u64,
}

/// Per-session lifecycle tunables (§3 session detection, §5 pending overflow).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionConfig {
    /// Inactivity duration after which a session is eligible for finalize.
    pub timeout_seconds: u64,
    /// Pending-fact count that triggers an early session-end (§5 overflow).
    #[serde(default)]
    pub pending_overflow_threshold: usize,
    /// Watcher scan cadence. The session watcher wakes every
    /// `scan_interval_seconds` to look for expired / overflowed sessions and
    /// trigger FinalizeSession.
    #[serde(default)]
    pub scan_interval_seconds: u64,
}

/// Native ort + ONNX Runtime backend configuration.
///
/// Holds the two adapter-boundary concerns the native NLI backend needs: the
/// Hugging Face model id to load and the local directory used to cache the
/// ONNX model + tokenizer artifacts. Both belong here, not in `smos-domain`
/// (which stays IO-free).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NliBackendConfig {
    /// Hugging Face model id downloaded by the native backend. The default
    /// matches the POC's benchmark winner (DeBERTa-v3 large, MNLI + FEVER +
    /// ANLI + ling-wanli).
    pub model: String,
    /// Local directory used to cache the ONNX model + tokenizer artifacts
    /// downloaded from HF Hub. The native backend writes a flat
    /// `model_quantized.onnx` + `tokenizer.json` here.
    pub cache_dir: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8888,
            shutdown_extraction_grace_seconds: 30,
            enable_response_extraction: true,
            graceful_degradation: true,
            log_format: "json".into(),
        }
    }
}

impl Default for SurrealConfig {
    fn default() -> Self {
        Self {
            path: "./data/smos.db".into(),
            namespace: "smos".into(),
            database: "smos".into(),
        }
    }
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        Self {
            url: "http://localhost:11434/v1/chat/completions".into(),
            api_key: "ollama".into(),
            auth_header: "Authorization".into(),
            timeout_seconds: 120,
        }
    }
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            url: "http://localhost:11434".into(),
            embedding_model: "hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:latest"
                .into(),
            extraction_model: "qwen3.5:2b".into(),
            timeout_seconds: 30,
            extraction_seed: 42,
            extraction_temperature: 0.0,
        }
    }
}

impl Default for RerankerConfig {
    fn default() -> Self {
        Self {
            url: "http://localhost:8181".into(),
            model: "qwen3-reranker".into(),
            timeout_seconds: 60,
        }
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: 1800,
            pending_overflow_threshold: 20,
            scan_interval_seconds: 60,
        }
    }
}

impl Default for NliBackendConfig {
    fn default() -> Self {
        Self {
            model: "MoritzLaurer/DeBERTa-v3-large-mnli-fever-anli-ling-wanli".into(),
            cache_dir: "./data/nli_cache".into(),
        }
    }
}

impl SmosConfig {
    /// Load from a TOML file (overridden by `SMOS__*` environment variables).
    /// Returns defaults when the file is missing so the proxy runs
    /// out-of-the-box without a config file; sections absent from a partial
    /// file also fall back to their defaults via `#[serde(default)]`.
    ///
    /// Environment overrides use the `SMOS__` prefix and a `__` section
    /// separator, so `SMOS__UPSTREAM__API_KEY=sk-...` overrides
    /// `[upstream].api_key` — keeps secrets out of the on-disk TOML.
    ///
    /// Emits a `tracing::warn!` when the on-disk file contains a legacy
    /// `[nli_sidecar]` section. The section is silently ignored at the
    /// serde layer (the field was removed when the Python sidecar was
    /// deleted), but values previously set there — most importantly `model`
    /// and `cache_dir` — would otherwise drop without trace. The warning
    /// points the operator at `[nli_backend]`, the replacement section.
    pub fn load(path: &str) -> Result<Self, ::config::ConfigError> {
        let mut builder = ::config::Config::builder();
        let file_exists = std::path::Path::new(path).exists();
        if file_exists {
            builder = builder.add_source(::config::File::with_name(path));
            warn_on_legacy_nli_sidecar_section(path);
        }
        builder = builder.add_source(::config::Environment::with_prefix("SMOS").separator("__"));
        builder.build()?.try_deserialize()
    }
}

/// Detect a legacy `[nli_sidecar]` section in the on-disk TOML and emit a
/// `tracing::warn!` naming its replacement. Substring match is intentional:
/// `toml` is not in the workspace dependency tree and the section header is
/// distinctive enough that a false positive (`nli_sidecar` appearing inside
/// a comment or string literal) is harmless — the warning only points the
/// operator at `[nli_backend]`, it does not change behaviour.
fn warn_on_legacy_nli_sidecar_section(path: &str) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    if raw.contains("[nli_sidecar]") {
        tracing::warn!(
            config_path = path,
            "the `[nli_sidecar]` section is no longer read; \
             move `model` and `cache_dir` into `[nli_backend]` \
             (the Python sidecar was removed — NLI is now native ort + ONNX). \
             Until then, `[nli_backend]` falls back to its built-in defaults."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Env vars are process-global; config tests that exercise `SMOS__*`
    // overrides must not run concurrently with other config loads. A single
    // module-level mutex serialises every config test for env-safety.
    static CONFIG_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn _lock() -> std::sync::MutexGuard<'static, ()> {
        CONFIG_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn default_has_canonical_values() {
        let _g = _lock();
        let cfg = SmosConfig::default();
        assert_eq!(cfg.server.port, 8888);
        assert_eq!(cfg.server.host, "127.0.0.1");
        assert_eq!(cfg.upstream.timeout_seconds, 120);
        assert_eq!(cfg.surreal.namespace, "smos");
        assert_eq!(cfg.nli.contradiction_threshold, 0.5);
        assert_eq!(cfg.nli.entailment_threshold, 0.6);
        assert!(cfg.nli_backend.model.starts_with("MoritzLaurer/"));
        assert_eq!(cfg.nli_backend.cache_dir, "./data/nli_cache");
    }

    #[test]
    fn load_missing_file_falls_back_to_defaults() {
        let _g = _lock();
        let cfg = SmosConfig::load("definitely-does-not-exist.toml").expect("defaults");
        assert_eq!(cfg.server.port, 8888);
    }

    #[test]
    fn load_partial_file_fills_missing_sections_from_defaults() {
        let _g = _lock();
        let tmp = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("tempfile");
        std::fs::write(tmp.path(), "[server]\nhost = \"0.0.0.0\"\nport = 9999\n").expect("write");
        let cfg = SmosConfig::load(tmp.path().to_str().unwrap()).expect("parse");
        assert_eq!(cfg.server.host, "0.0.0.0");
        assert_eq!(cfg.server.port, 9999);
        assert_eq!(cfg.upstream.timeout_seconds, 120);
        assert_eq!(cfg.surreal.namespace, "smos");
    }

    #[test]
    fn load_full_file_overrides_all_sections() {
        let _g = _lock();
        let toml = "[surreal]\npath = \"./x.db\"\nnamespace = \"ns\"\ndatabase = \"db\"\n\
                    [server]\nhost = \"h\"\nport = 1\nshutdown_extraction_grace_seconds = 5\n\
                    enable_response_extraction = false\ngraceful_degradation = false\nlog_format = \"pretty\"\n\
                    [upstream]\nurl = \"u\"\napi_key = \"k\"\nauth_header = \"api-key\"\ntimeout_seconds = 9\n\
                     [ollama]\nurl = \"http://ollama:11434\"\nembedding_model = \"m1\"\n\
                     extraction_model = \"m2\"\ntimeout_seconds = 11\n\
                     extraction_seed = 7\nextraction_temperature = 0.2\n\
                    [reranker]\nurl = \"http://reranker:8181\"\nmodel = \"rr\"\ntimeout_seconds = 7\n\
                    [retrieval]\ntop_k_initial = 30\ntop_k_final = 3\nmin_confidence = 0.6\nmin_topic_chars = 2\n\
                    [merge]\ncosine_threshold = 0.8\n\
                    [confidence]\nbase = 0.4\nmulti_source_bonus = 0.1\nno_contradiction_bonus = 0.05\naccept_threshold = 0.65\npending_threshold = 0.3\n\
                    [heat]\ndecay_rate = 0.02\nmin_threshold = 0.15\n\
                     [nli]\ncontradiction_threshold = 0.55\nentailment_threshold = 0.65\n\
                     [extraction]\ndedup_cosine_threshold = 0.92\n\
                     [nli_backend]\nmodel = \"cross-encoder/nli-deberta-v3\"\ncache_dir = \"/var/cache/smos/nli\"\n\
                    [session]\ntimeout_seconds = 600\npending_overflow_threshold = 15\nscan_interval_seconds = 30\n";
        let tmp = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("tempfile");
        std::fs::write(tmp.path(), toml).expect("write");
        let cfg = SmosConfig::load(tmp.path().to_str().unwrap()).expect("parse");
        assert_eq!(cfg.server.host, "h");
        assert_eq!(cfg.server.port, 1);
        assert!(!cfg.server.enable_response_extraction);
        assert_eq!(cfg.server.log_format, "pretty");
        assert_eq!(cfg.upstream.auth_header, "api-key");
        assert_eq!(cfg.upstream.timeout_seconds, 9);
        assert_eq!(cfg.surreal.path, "./x.db");
        assert_eq!(cfg.ollama.url, "http://ollama:11434");
        assert_eq!(cfg.ollama.embedding_model, "m1");
        assert_eq!(cfg.ollama.timeout_seconds, 11);
        assert_eq!(cfg.ollama.extraction_seed, 7);
        assert_eq!(cfg.ollama.extraction_temperature, 0.2);
        assert_eq!(cfg.reranker.url, "http://reranker:8181");
        assert_eq!(cfg.reranker.model, "rr");
        assert_eq!(cfg.reranker.timeout_seconds, 7);
        assert_eq!(cfg.retrieval.top_k_initial, 30);
        assert_eq!(cfg.retrieval.top_k_final, 3);
        assert_eq!(cfg.merge.cosine_threshold, 0.8);
        assert_eq!(cfg.confidence.accept_threshold, 0.65);
        assert_eq!(cfg.heat.min_threshold, 0.15);
        assert_eq!(cfg.nli.contradiction_threshold, 0.55);
        assert_eq!(cfg.nli.entailment_threshold, 0.65);
        assert_eq!(cfg.extraction.dedup_cosine_threshold, 0.92);
        assert_eq!(cfg.nli_backend.model, "cross-encoder/nli-deberta-v3");
        assert_eq!(cfg.nli_backend.cache_dir, "/var/cache/smos/nli");
        assert_eq!(cfg.session.timeout_seconds, 600);
        assert_eq!(cfg.session.pending_overflow_threshold, 15);
        assert_eq!(cfg.session.scan_interval_seconds, 30);
    }

    /// Backwards-compatibility shim: existing `smos.toml` files in the field
    /// may still carry an `[nli_sidecar]` section. Serde's `deny_unknown_fields`
    /// is intentionally OFF, so the legacy section is silently ignored rather
    /// than aborting startup. This test pins that behaviour.
    #[test]
    fn legacy_nli_sidecar_section_is_ignored_without_failing_load() {
        let _g = _lock();
        let toml = "[server]\nport = 7777\n\
                    [nli_sidecar]\npython = \"python\"\nscript = \"x.py\"\n\
                    model = \"legacy\"\ndevice = \"cpu\"\nrequest_timeout_secs = 1\n\
                    ready_timeout_secs = 1\nmax_restarts_in_window = 1\nrestart_window_secs = 1\n\
                    implementation = \"native\"\ncache_dir = \"./legacy\"\n";
        let tmp = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("tempfile");
        std::fs::write(tmp.path(), toml).expect("write");
        let cfg = SmosConfig::load(tmp.path().to_str().unwrap()).expect("parse");
        assert_eq!(cfg.server.port, 7777);
        // nli_backend falls back to the default — the legacy section does not
        // feed it.
        assert!(cfg.nli_backend.model.starts_with("MoritzLaurer/"));
    }

    #[test]
    fn new_sections_default_when_omitted_from_partial_file() {
        let _g = _lock();
        let tmp = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("tempfile");
        std::fs::write(tmp.path(), "[server]\nport = 7777\n").expect("write");
        let cfg = SmosConfig::load(tmp.path().to_str().unwrap()).expect("parse");
        assert_eq!(cfg.server.port, 7777);
        assert_eq!(cfg.ollama.timeout_seconds, 30);
        assert_eq!(cfg.reranker.model, "qwen3-reranker");
        assert_eq!(cfg.retrieval.top_k_final, 5);
        assert_eq!(cfg.session.pending_overflow_threshold, 20);
    }

    #[test]
    fn env_var_overrides_file_value() {
        let _g = _lock();
        let tmp = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("tempfile");
        std::fs::write(tmp.path(), "[upstream]\napi_key = \"from-file\"\n").expect("write");
        let prior = std::env::var("SMOS__UPSTREAM__API_KEY").ok();
        // SAFETY: this test holds `CONFIG_TEST_LOCK`, which serialises every
        // config test in this binary, so no concurrent `SmosConfig::load`
        // (which reads the SMOS__ namespace) can race with this mutation.
        unsafe {
            std::env::set_var("SMOS__UPSTREAM__API_KEY", "sk-from-env");
        }
        let cfg = SmosConfig::load(tmp.path().to_str().unwrap()).expect("parse");
        // SAFETY: same serialisation guarantee as above.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("SMOS__UPSTREAM__API_KEY", v),
                None => std::env::remove_var("SMOS__UPSTREAM__API_KEY"),
            }
        }
        assert_eq!(cfg.upstream.api_key, "sk-from-env");
    }

    #[test]
    fn config_roundtrips_through_serde_json() {
        let _g = _lock();
        let cfg = SmosConfig::default();
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: SmosConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.server.port, cfg.server.port);
        assert_eq!(back.upstream.url, cfg.upstream.url);
    }
}
