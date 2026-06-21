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
//!
//! # Section map
//!
//! | TOML section        | Rust field                 | Notes                          |
//!|---------------------|----------------------------|--------------------------------|
//! | `[surreal]`         | [`SurrealConfig`]          |                                |
//! | `[server]`          | [`ServerConfig`]           |                                |
//! | `[upstream]`        | [`UpstreamConfig`]         | Multi-provider via `[[upstream.providers]]`. |
//! | `[llm_extraction]`  | [`LlmExtractionConfig`]    |                                |
//! | `[embedding]`       | [`EmbeddingConfig`]        |                                |
//! | `[reranker]`        | [`RerankerConfig`]         |                                |
//! | `[retrieval]`       | [`RetrievalConfig`]        | Re-exported from `smos-domain`.|
//! | `[merge]`           | [`MergeConfig`]            | Re-exported from `smos-domain`.|
//! | `[confidence]`      | [`ConfidenceConfig`]       | Re-exported from `smos-domain`.|
//! | `[heat]`            | [`HeatConfig`]             | Re-exported from `smos-domain`.|
//! | `[nli]`             | [`NliConfig`]              | Domain verdict thresholds.     |
//! | `[nli_backend]`     | [`NliBackendConfig`]       | Adapter-only: native ort/ONNX `model` + `cache_dir`. |
//! | `[extraction]`      | [`ExtractionConfig`]       | Re-exported from `smos-domain`.|
//! | `[session]`         | [`SessionConfig`]          |                                |

use serde::{Deserialize, Serialize};
pub use smos_domain::config::{
    ConfidenceConfig, ExtractionConfig, HeatConfig, MergeConfig, NliConfig, RetrievalConfig,
};

/// Root configuration.
///
/// Sections that originate in `smos-domain` (`retrieval`, `merge`,
/// `confidence`, `heat`, `nli`) are re-exported from this module so callers
/// have a single import path. Sections that only make sense at the adapter
/// boundary (`surreal`, `server`, `upstream`, `llm_extraction`, `embedding`,
/// `reranker`, `session`) live here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SmosConfig {
    #[serde(default)]
    pub surreal: SurrealConfig,

    #[serde(default)]
    pub server: ServerConfig,

    #[serde(default)]
    pub upstream: UpstreamConfig,

    /// Provider-agnostic config for the fact-extraction LLM
    /// (`/api/chat`-style endpoint).
    #[serde(default)]
    pub llm_extraction: LlmExtractionConfig,

    /// Provider-agnostic config for the embedding model.
    #[serde(default)]
    pub embedding: EmbeddingConfig,

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

    /// NLI verdict thresholds (domain layer). Drives the
    /// `is_contradiction` / `is_entailment` / `decide_merge` predicates.
    #[serde(default)]
    pub nli: NliConfig,

    /// Native ort/ONNX backend for NLI inference. Adapter-only: the model id
    /// and cache directory are interpreter-level data that the domain layer
    /// never reads — keeping them out of `smos-domain::NliConfig` preserves
    /// the layering invariant ("domain types carry no IO-boundary data").
    #[serde(default)]
    pub nli_backend: NliBackendConfig,

    /// Semantic dedup safety net for fact extraction (`persist_facts` step 2).
    /// Backs the cosine-similarity gate the extractor falls back to when
    /// `FactId = SHA1(content)` exact match misses a rephrased re-observation.
    #[serde(default)]
    pub extraction: ExtractionConfig,

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

/// Upstream chat-completion proxy config.
///
/// Declares the provider pool via `[[upstream.providers]]` plus a routing
/// [`UpstreamStrategy`]. A minimal config:
///
/// ```toml
/// [[upstream.providers]]
/// name = "ollama-local"
/// url = "http://localhost:11434/v1/chat/completions"
/// api_key = "ollama"
/// auth_header = "Authorization"
/// timeout_seconds = 120
///
/// [upstream.strategy]
/// mode = "single"   # or "round_robin" / "failover"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UpstreamConfig {
    #[serde(default)]
    pub providers: Vec<UpstreamProvider>,
    #[serde(default)]
    pub strategy: UpstreamStrategy,
}

/// One upstream LLM provider entry. Multiple providers can be declared via
/// `[[upstream.providers]]`; the active one is chosen by
/// [`UpstreamStrategy`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamProvider {
    /// Operator-facing identifier used in logs (`upstream failed, trying next:
    /// <name>`). Defaults to the URL if omitted in TOML.
    pub name: String,
    /// Full chat-completions URL (with path).
    pub url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_auth_header")]
    pub auth_header: String,
    #[serde(default = "default_upstream_timeout")]
    pub timeout_seconds: u64,
}

/// Routing strategy across [`UpstreamConfig::providers`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UpstreamStrategy {
    /// `"round_robin"` (default), `"failover"`, or `"single"`. Unknown values
    /// fall back to `single` at the adapter call site so a typo never silently
    /// enables an unintended strategy.
    pub mode: String,
}

/// LLM fact-extraction endpoint config (provider-agnostic).
///
/// Backs the post-response extraction pipeline. The endpoint is expected to
/// be Ollama's `/api/chat` shape (`{model, messages, options: {temperature,
/// seed}}`); cloud providers are supported as long as they accept that
/// request envelope. For OpenAI `/v1/chat/completions` shapes, use the
/// main [`UpstreamConfig`] instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmExtractionConfig {
    /// API base URL (no path suffix). The extractor appends `/api/chat`.
    pub url: String,
    /// Model id passed in the `model` field of `/api/chat`.
    pub model: String,
    /// Optional API key for cloud providers (Ollama ignores the field).
    #[serde(default)]
    pub api_key: String,
    /// Per-request HTTP timeout.
    pub timeout_seconds: u64,
    /// Sampling temperature passed to `options.temperature`. `0.0` (greedy
    /// decoding) is the near-deterministic baseline.
    pub temperature: f32,
    /// Sampling seed passed to `options.seed`. Pairing `temperature = 0.0`
    /// with a pinned `seed` makes the extractor re-yield the same bullet
    /// list across runs on the same backend.
    pub seed: u32,
}

/// Embedding endpoint config (provider-agnostic).
///
/// Backs the topic-embedding step of the enrich pipeline. The endpoint is
/// expected to be Ollama's `/api/embeddings` shape (`{model, prompt}`); cloud
/// providers are supported as long as they accept that envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbeddingConfig {
    /// API base URL (no path suffix). The adapter appends `/api/embeddings`.
    /// May differ from [`LlmExtractionConfig::url`] so the embedder can run
    /// on a different host (or a different provider entirely).
    pub url: String,
    /// Model id passed in the `model` field of `/api/embeddings`.
    pub model: String,
    /// Vector dimensionality. MUST match the HNSW index declared in
    /// `surreal_schema::FACT_DDL`. The default 1024 matches the canonical
    /// Jina v5 retrieval-GGUF config; override only if you re-index.
    pub dimensions: usize,
    /// Optional API key for cloud providers (Ollama ignores the field).
    #[serde(default)]
    pub api_key: String,
    /// Per-request HTTP timeout.
    pub timeout_seconds: u64,
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

/// Native ort/ONNX backend for NLI inference — adapter-only sibling of the
/// domain [`NliConfig`].
///
/// The domain layer never interprets `model` or `cache_dir`; they are read
/// exactly once at startup by [`crate::nli::build_classifier`] and passed to
/// the ort session build. Keeping them in this adapter-side struct (rather
/// than the domain `NliConfig`) preserves the "domain carries no
/// IO-boundary data" invariant.
///
/// `deny_unknown_fields` mirrors the domain `NliConfig`: a typo here fails
/// loudly at startup instead of silently dropping the configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
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

// ---------------------------------------------------------------------------
// Default impls
// ---------------------------------------------------------------------------

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

impl Default for UpstreamStrategy {
    fn default() -> Self {
        Self {
            mode: default_strategy_mode().into(),
        }
    }
}

impl Default for LlmExtractionConfig {
    fn default() -> Self {
        Self {
            url: "http://localhost:11434".into(),
            model: "qwen3.5:2b".into(),
            api_key: String::new(),
            timeout_seconds: 30,
            temperature: 0.0,
            seed: 42,
        }
    }
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            url: "http://localhost:11434".into(),
            model: "hf.co/jinaai/jinaai-embeddings-v5-text-small-retrieval-GGUF:latest".into(),
            dimensions: 1024,
            api_key: String::new(),
            timeout_seconds: 30,
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

impl Default for NliBackendConfig {
    fn default() -> Self {
        Self {
            model: "MoritzLaurer/DeBERTa-v3-large-mnli-fever-anli-ling-wanli".into(),
            cache_dir: "./data/nli_cache".into(),
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

// ---------------------------------------------------------------------------
// Defaults for serde `default = "..."` attributes
// ---------------------------------------------------------------------------

fn default_auth_header() -> String {
    "Authorization".into()
}

fn default_upstream_timeout() -> u64 {
    120
}

fn default_strategy_mode() -> &'static str {
    "round_robin"
}

// ---------------------------------------------------------------------------
// UpstreamConfig helpers
// ---------------------------------------------------------------------------

impl UpstreamProvider {
    /// Construct with the canonical defaults for the optional fields.
    pub fn new(name: &str, url: &str) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            api_key: String::new(),
            auth_header: default_auth_header(),
            timeout_seconds: default_upstream_timeout(),
        }
    }
}

// ---------------------------------------------------------------------------
// SmosConfig loading
// ---------------------------------------------------------------------------

impl SmosConfig {
    /// Load from a TOML file (overridden by `SMOS__*` environment variables).
    /// Returns defaults when the file is missing so the proxy runs
    /// out-of-the-box without a config file; sections absent from a partial
    /// file also fall back to their defaults via `#[serde(default)]`.
    ///
    /// Environment overrides use the `SMOS__` prefix and a `__` section
    /// separator. For the multi-provider shape (`[[upstream.providers]]`),
    /// the convenience env var `SMOS__UPSTREAM__API_KEY` is broadcast onto
    /// every provider entry whose TOML `api_key` was left empty — so an
    /// operator can keep all per-provider secrets out of the on-disk TOML
    /// by writing `api_key = ""` next to each provider and exporting
    /// `SMOS__UPSTREAM__API_KEY`. Per-provider overrides still work via
    /// `SMOS__UPSTREAM__PROVIDERS__<idx>__API_KEY`.
    pub fn load(path: &str) -> Result<Self, ::config::ConfigError> {
        let mut builder = ::config::Config::builder();
        if std::path::Path::new(path).exists() {
            builder = builder.add_source(::config::File::with_name(path));
        }
        builder = builder.add_source(::config::Environment::with_prefix("SMOS").separator("__"));
        let mut cfg: SmosConfig = builder.build()?.try_deserialize()?;
        apply_env_api_key_to_providers(&mut cfg);
        Ok(cfg)
    }
}

/// Apply `SMOS__UPSTREAM__API_KEY` to multi-provider entries.
///
/// Reads the env var directly so the broadcast can detect "set but empty"
/// vs "unset" reliably.
///
/// # Dual-path note
///
/// The `config` crate also sees `SMOS__UPSTREAM__API_KEY` and tries to map
/// it to `upstream.api_key`. That field no longer exists on
/// [`UpstreamConfig`] (the legacy single-provider shape was removed), so
/// the `config` crate silently drops the value. This function then reads
/// the env var a second time via [`std::env::var`] and broadcasts it onto
/// every `[[upstream.providers]]` entry whose TOML `api_key == ""`. The
/// double-read is intentional: per-provider env overrides
/// (`SMOS__UPSTREAM__PROVIDERS__<idx>__API_KEY`) still flow through the
/// `config` crate, while the convenience broadcast key flows through here.
///
/// Broadcast rule: when the env var is non-empty, every provider in
/// `cfg.upstream.providers` whose TOML `api_key == ""` inherits the env
/// value. Providers that already carry a non-empty TOML key keep theirs;
/// a `tracing::warn!` is emitted in that case so the operator notices the
/// env var was a no-op for those providers (likely a stale export).
fn apply_env_api_key_to_providers(cfg: &mut SmosConfig) {
    let Ok(env_key) = std::env::var("SMOS__UPSTREAM__API_KEY") else {
        return;
    };
    if env_key.is_empty() {
        return;
    }
    let mut any_skipped = false;
    for provider in cfg.upstream.providers.iter_mut() {
        if provider.api_key.is_empty() {
            provider.api_key = env_key.clone();
        } else {
            any_skipped = true;
        }
    }
    if any_skipped {
        tracing::warn!(
            env_var = "SMOS__UPSTREAM__API_KEY",
            "the env-supplied upstream api_key was NOT applied to every \
             [[upstream.providers]] entry because at least one provider already \
             carries a non-empty `api_key` in TOML. Clear those entries \
             (`api_key = \"\"`) to let the env var take effect, or remove \
             the env var."
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
        assert!(cfg.upstream.providers.is_empty());
        assert_eq!(cfg.surreal.namespace, "smos");
        assert_eq!(cfg.nli.contradiction_threshold, 0.5);
        assert_eq!(cfg.nli.entailment_threshold, 0.6);
        assert!(cfg.nli_backend.model.starts_with("MoritzLaurer/"));
        assert_eq!(cfg.nli_backend.cache_dir, "./data/nli_cache");
        assert_eq!(cfg.llm_extraction.model, "qwen3.5:2b");
        assert_eq!(cfg.llm_extraction.seed, 42);
        assert_eq!(cfg.embedding.dimensions, 1024);
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
        assert_eq!(cfg.surreal.namespace, "smos");
    }

    #[test]
    fn load_full_file_overrides_all_sections() {
        let _g = _lock();
        let toml = "[surreal]\npath = \"./x.db\"\nnamespace = \"ns\"\ndatabase = \"db\"\n\
                    [server]\nhost = \"h\"\nport = 1\nshutdown_extraction_grace_seconds = 5\n\
                    enable_response_extraction = false\ngraceful_degradation = false\nlog_format = \"pretty\"\n\
                    [[upstream.providers]]\nname = \"u\"\nurl = \"u\"\napi_key = \"k\"\nauth_header = \"api-key\"\ntimeout_seconds = 9\n\
                    [upstream.strategy]\nmode = \"single\"\n\
                    [llm_extraction]\nurl = \"http://llm:11434\"\nmodel = \"qwen\"\ntimeout_seconds = 11\n\
                    temperature = 0.2\nseed = 7\n\
                    [embedding]\nurl = \"http://embed:11434\"\nmodel = \"jina\"\ndimensions = 768\ntimeout_seconds = 11\n\
                    [reranker]\nurl = \"http://reranker:8181\"\nmodel = \"rr\"\ntimeout_seconds = 7\n\
                    [retrieval]\ntop_k_initial = 30\ntop_k_final = 3\nmin_confidence = 0.6\nmin_topic_chars = 2\n\
                    [merge]\ncosine_threshold = 0.8\n\
                    [confidence]\nbase = 0.4\nmulti_source_bonus = 0.1\nno_contradiction_bonus = 0.05\naccept_threshold = 0.65\npending_threshold = 0.3\n\
                    [heat]\ndecay_rate = 0.02\nmin_threshold = 0.15\n\
                    [nli]\ncontradiction_threshold = 0.55\nentailment_threshold = 0.65\n\
                    [nli_backend]\nmodel = \"cross-encoder/nli-deberta-v3\"\ncache_dir = \"/var/cache/smos/nli\"\n\
                    [extraction]\ndedup_cosine_threshold = 0.92\n\
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
        assert_eq!(cfg.upstream.providers.len(), 1);
        assert_eq!(cfg.upstream.providers[0].auth_header, "api-key");
        assert_eq!(cfg.upstream.providers[0].timeout_seconds, 9);
        assert_eq!(cfg.upstream.strategy.mode, "single");
        assert_eq!(cfg.surreal.path, "./x.db");
        assert_eq!(cfg.llm_extraction.url, "http://llm:11434");
        assert_eq!(cfg.llm_extraction.model, "qwen");
        assert_eq!(cfg.llm_extraction.timeout_seconds, 11);
        assert_eq!(cfg.llm_extraction.seed, 7);
        assert_eq!(cfg.llm_extraction.temperature, 0.2);
        assert_eq!(cfg.embedding.url, "http://embed:11434");
        assert_eq!(cfg.embedding.model, "jina");
        assert_eq!(cfg.embedding.dimensions, 768);
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
        assert_eq!(cfg.nli_backend.model, "cross-encoder/nli-deberta-v3");
        assert_eq!(cfg.nli_backend.cache_dir, "/var/cache/smos/nli");
        assert_eq!(cfg.extraction.dedup_cosine_threshold, 0.92);
        assert_eq!(cfg.session.timeout_seconds, 600);
        assert_eq!(cfg.session.pending_overflow_threshold, 15);
        assert_eq!(cfg.session.scan_interval_seconds, 30);
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
        assert_eq!(cfg.llm_extraction.timeout_seconds, 30);
        assert!(cfg.embedding.model.starts_with("hf.co/jinaai"));
        assert_eq!(cfg.reranker.model, "qwen3-reranker");
        assert_eq!(cfg.retrieval.top_k_final, 5);
        assert_eq!(cfg.session.pending_overflow_threshold, 20);
    }

    #[test]
    fn config_roundtrips_through_serde_json() {
        let _g = _lock();
        let cfg = SmosConfig::default();
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: SmosConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.server.port, cfg.server.port);
        assert_eq!(back.upstream.providers.len(), cfg.upstream.providers.len());
    }

    // --- Upstream multi-provider behaviour ------------------------------

    #[test]
    fn upstream_strategy_default_is_round_robin() {
        assert_eq!(default_strategy_mode(), "round_robin");
        assert_eq!(UpstreamStrategy::default().mode, "round_robin");
    }

    /// Multi-provider TOML shape parses into `providers` + `strategy`.
    #[test]
    fn multi_provider_upstream_section_parses() {
        let _g = _lock();
        let toml = "[[upstream.providers]]\n\
                    name = \"ollama-local\"\n\
                    url = \"http://localhost:11434/v1/chat/completions\"\n\
                    api_key = \"ollama\"\n\
                    auth_header = \"Authorization\"\n\
                    timeout_seconds = 120\n\
                    [[upstream.providers]]\n\
                    name = \"openrouter\"\n\
                    url = \"https://openrouter.ai/api/v1/chat/completions\"\n\
                    api_key = \"sk-or-xxx\"\n\
                    timeout_seconds = 90\n\
                    [upstream.strategy]\n\
                    mode = \"failover\"\n";
        let tmp = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("tempfile");
        std::fs::write(tmp.path(), toml).expect("write");
        let cfg = SmosConfig::load(tmp.path().to_str().unwrap()).expect("parse");
        let list = cfg.upstream.providers;
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "ollama-local");
        assert_eq!(list[1].name, "openrouter");
        assert_eq!(list[1].api_key, "sk-or-xxx");
        // Second provider inherits the default `auth_header` since the TOML
        // omits it.
        assert_eq!(list[1].auth_header, "Authorization");
        assert_eq!(cfg.upstream.strategy.mode, "failover");
    }

    /// `SMOS__UPSTREAM__API_KEY` is broadcast onto every multi-provider
    /// entry whose TOML `api_key == ""`, preserving the documented "secrets
    /// out of TOML" contract.
    #[test]
    fn env_api_key_broadcasts_to_empty_provider_entries() {
        let _g = _lock();
        let toml = "[[upstream.providers]]\n\
                    name = \"p1\"\n\
                    url = \"http://p1\"\n\
                    api_key = \"\"\n\
                    [[upstream.providers]]\n\
                    name = \"p2\"\n\
                    url = \"http://p2\"\n\
                    api_key = \"\"\n";
        let tmp = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("tempfile");
        std::fs::write(tmp.path(), toml).expect("write");
        let prior = std::env::var("SMOS__UPSTREAM__API_KEY").ok();
        // SAFETY: this test holds `CONFIG_TEST_LOCK`, which serialises every
        // config test in this binary.
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
        let providers = cfg.upstream.providers;
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].api_key, "sk-from-env");
        assert_eq!(providers[1].api_key, "sk-from-env");
    }

    /// Provider entries that already carry a non-empty TOML `api_key` keep
    /// theirs and the env var does NOT silently overwrite them.
    #[test]
    fn env_api_key_does_not_overwrite_explicit_provider_key() {
        let _g = _lock();
        let toml = "[[upstream.providers]]\n\
                    name = \"p1\"\n\
                    url = \"http://p1\"\n\
                    api_key = \"from-toml\"\n";
        let tmp = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("tempfile");
        std::fs::write(tmp.path(), toml).expect("write");
        let prior = std::env::var("SMOS__UPSTREAM__API_KEY").ok();
        // SAFETY: this test holds `CONFIG_TEST_LOCK`.
        unsafe {
            std::env::set_var("SMOS__UPSTREAM__API_KEY", "sk-from-env");
        }
        let cfg = SmosConfig::load(tmp.path().to_str().unwrap()).expect("parse");
        // SAFETY: same serialisation guarantee.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("SMOS__UPSTREAM__API_KEY", v),
                None => std::env::remove_var("SMOS__UPSTREAM__API_KEY"),
            }
        }
        let providers = cfg.upstream.providers;
        assert_eq!(providers[0].api_key, "from-toml");
    }

    // --- Legacy section guards -----------------------------------------
    //
    // The legacy bridges (`apply_legacy_ollama_section`, `apply_nli_section_merge`,
    // `UpstreamConfig::effective_providers`, etc.) were removed per the
    // "old smos.toml configs MUST NOT work" contract. These tests pin the
    // intentional behaviour: a TOML carrying legacy sections/fields still
    // LOADS (serde has no `deny_unknown_fields`) but the legacy values
    // NEVER affect the canonical config. A future engineer who re-adds a
    // bridge will break one of these tests, which is the point — the
    // intent is documented in code, not just in commit history.

    /// A leftover `[ollama]` section does NOT populate `[llm_extraction]` /
    /// `[embedding]`. The legacy fields are silently dropped at deserialize
    /// time and the canonical sections keep their defaults.
    #[test]
    fn legacy_ollama_section_does_not_bridge_into_canonical_sections() {
        let _g = _lock();
        let toml = "[ollama]\n\
                    url = \"http://legacy:11434\"\n\
                    embedding_model = \"legacy-embed\"\n\
                    extraction_model = \"legacy-extract\"\n\
                    timeout_seconds = 17\n";
        let tmp = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("tempfile");
        std::fs::write(tmp.path(), toml).expect("write");
        let cfg = SmosConfig::load(tmp.path().to_str().unwrap()).expect("parse");
        // Defaults preserved — legacy fields did NOT bleed through.
        assert_eq!(cfg.llm_extraction.url, "http://localhost:11434");
        assert_eq!(cfg.llm_extraction.model, "qwen3.5:2b");
        assert_eq!(cfg.llm_extraction.timeout_seconds, 30);
        assert!(cfg.embedding.model.starts_with("hf.co/jinaai"));
        assert_eq!(cfg.embedding.timeout_seconds, 30);
    }

    /// `[nli_backend]` is the CANONICAL adapter-side section (carrying
    /// `model` + `cache_dir`); the domain-side `[nli]` section now holds
    /// only verdict thresholds. This test pins the layering invariant: an
    /// operator-supplied `[nli_backend]` populates `cfg.nli_backend`, and
    /// `cfg.nli` (the domain thresholds) stays at its defaults unless the
    /// operator also overrides `[nli]`.
    #[test]
    fn nli_backend_section_is_canonical_and_does_not_touch_domain_thresholds() {
        let _g = _lock();
        let toml = "[nli_backend]\n\
                    model = \"cross-encoder/nli-deberta-v3\"\n\
                    cache_dir = \"/var/cache/smos/nli\"\n";
        let tmp = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("tempfile");
        std::fs::write(tmp.path(), toml).expect("write");
        let cfg = SmosConfig::load(tmp.path().to_str().unwrap()).expect("parse");
        // Adapter section picked up the override.
        assert_eq!(cfg.nli_backend.model, "cross-encoder/nli-deberta-v3");
        assert_eq!(cfg.nli_backend.cache_dir, "/var/cache/smos/nli");
        // Domain thresholds stayed at their defaults — the layering
        // invariant is intact.
        assert_eq!(cfg.nli.contradiction_threshold, 0.5);
        assert_eq!(cfg.nli.entailment_threshold, 0.6);
    }

    /// Putting `model` (an adapter-only field) under `[nli]` MUST fail loudly
    /// at startup. `NliConfig` carries `#[serde(deny_unknown_fields)]` so the
    /// parser rejects the misplacement instead of silently dropping it — the
    /// explicit safety mechanism that justifies splitting the sections in
    /// the first place. A future refactor that removes the attribute will
    /// break this test, which is the point: the loud-failure contract is
    /// pinned in CI, not just in code review.
    #[test]
    fn nli_section_with_adapter_field_fails_loudly() {
        let _g = _lock();
        let toml = "[nli]\n\
                    contradiction_threshold = 0.5\n\
                    entailment_threshold = 0.6\n\
                    model = \"accidental-misplacement\"\n";
        let tmp = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("tempfile");
        std::fs::write(tmp.path(), toml).expect("write");
        let result = SmosConfig::load(tmp.path().to_str().unwrap());
        assert!(
            result.is_err(),
            "operator misplacing `model` under `[nli]` must fail loudly, not silently drop"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("model") && err_msg.contains("unknown"),
            "error must identify the unknown field; got: {err_msg}"
        );
    }

    /// Symmetric loud-failure for the adapter side: an unknown field under
    /// `[nli_backend]` MUST fail loudly. `NliBackendConfig` carries the same
    /// `#[serde(deny_unknown_fields)]` so a typo (`modle = "..."`) does not
    /// silently fall back to the default model.
    #[test]
    fn nli_backend_section_with_unknown_field_fails_loudly() {
        let _g = _lock();
        let toml = "[nli_backend]\n\
                    modle = \"typo-for-model\"\n\
                    cache_dir = \"./data/nli_cache\"\n";
        let tmp = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("tempfile");
        std::fs::write(tmp.path(), toml).expect("write");
        let result = SmosConfig::load(tmp.path().to_str().unwrap());
        assert!(
            result.is_err(),
            "typo in `[nli_backend]` must fail loudly, not silently fall back to defaults"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("modle") && err_msg.contains("unknown"),
            "error must identify the unknown field; got: {err_msg}"
        );
    }

    /// A leftover `[nli_sidecar]` section (Python sidecar, removed) does
    /// NOT abort startup and does NOT populate any field. Pinned so a
    /// future change that re-introduces sidecar parsing breaks this test.
    #[test]
    fn legacy_nli_sidecar_section_is_silently_ignored() {
        let _g = _lock();
        let toml = "[nli_sidecar]\n\
                    python = \"python\"\n\
                    script = \"x.py\"\n\
                    cache_dir = \"./legacy\"\n";
        let tmp = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("tempfile");
        std::fs::write(tmp.path(), toml).expect("write");
        let cfg = SmosConfig::load(tmp.path().to_str().unwrap()).expect("parse");
        assert_eq!(cfg.nli_backend.cache_dir, "./data/nli_cache");
    }

    /// The legacy single-provider shape (`[upstream].url` / `.api_key` /
    /// `.auth_header` / `.timeout_seconds` at the top level of the
    /// `[upstream]` section, with NO `[[upstream.providers]]` array) does
    /// NOT synthesise a provider. `cfg.upstream.providers` stays empty and
    /// the operator gets a startup error from `ReqwestUpstreamPool::new`
    /// ("no upstream providers configured") instead of a silently
    /// working legacy bridge.
    #[test]
    fn legacy_single_provider_upstream_fields_do_not_synthesise_a_provider() {
        let _g = _lock();
        let toml = "[upstream]\n\
                    url = \"http://legacy:11434/v1/chat/completions\"\n\
                    api_key = \"ollama\"\n\
                    auth_header = \"Authorization\"\n\
                    timeout_seconds = 120\n";
        let tmp = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("tempfile");
        std::fs::write(tmp.path(), toml).expect("write");
        let cfg = SmosConfig::load(tmp.path().to_str().unwrap()).expect("parse");
        // Legacy fields dropped — no provider synthesised.
        assert!(
            cfg.upstream.providers.is_empty(),
            "legacy [upstream].url must NOT auto-wrap into a provider entry"
        );
    }
}
