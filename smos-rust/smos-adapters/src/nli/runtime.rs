//! Shared runtime wiring for the NLI backend.
//!
//! Both the `serve` and `finalize` subcommands need to translate the
//! configuration into a concrete classifier. Centralising the build here
//! keeps the two callers symmetric and avoids divergent error handling.

use std::path::PathBuf;

use anyhow::Result;

use crate::config::SmosConfig;
use crate::nli::NativeNliClassifier;

/// Build a [`NativeNliClassifier`] from `config.nli_backend`.
///
/// Used by `smos finalize` and `smos serve`. The classifier owns the ort
/// session + tokenizer; constructing it once at startup avoids paying the
/// model-load cost per request.
pub async fn build_classifier(config: &SmosConfig) -> Result<NativeNliClassifier> {
    let classifier = NativeNliClassifier::new(
        &config.nli_backend.model,
        PathBuf::from(&config.nli_backend.cache_dir),
    )?;
    Ok(classifier)
}
