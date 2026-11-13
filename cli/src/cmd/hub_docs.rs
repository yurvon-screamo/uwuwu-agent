//! Hub-ветка doc-семьи (read/docs ls/search + write-path create/append/
//! replace/delete/tree): DocStore GitHubDocStore (git-клон uwuwu-hub).
//! Срез D, задача 20261112155302-ttyp35g; write-path — задача 46.

use crate::store::doc_store::{DocRef, DocStore, DocWrite};
use crate::store::github_doc_store::GitHubDocStore;
use anyhow::{bail, Result};
use std::io::Read;

fn doc_store() -> GitHubDocStore {
    GitHubDocStore::from_env(crate::cmd::run::hub_repo_default())
}

pub(crate) async fn read(slug: &str) -> Result<String> {
    doc_store()
        .read(&DocRef {
            slug: slug.to_string(),
        })
        .await
}

pub(crate) async fn docs_ls(prefix: &str) -> Result<()> {
    let entries = doc_store().ls(prefix).await?;
    if entries.is_empty() {
        println!("пусто");
        return Ok(());
    }
    for e in &entries {
        println!("{}", e.name);
    }
    Ok(())
}

/// Ввод тела/текста мутации: --stdin (до EOF) или --md (файл; гард на
/// существование — подсказка stdin).
fn body_input(stdin: bool, md: &Option<String>) -> Result<String> {
    match (stdin, md) {
        (true, _) => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            Ok(buf)
        }
        (false, Some(path)) => {
            if !std::path::Path::new(path).is_file() {
                bail!("--md ждёт путь к файлу; для текста из stdin — --stdin")
            }
            Ok(std::fs::read_to_string(path)?)
        }
        (false, None) => bail!("вход пуст: --stdin или --md <файл>"),
    }
}

fn print_write(doc: &str, warnings: &[String]) {
    for w in warnings {
        println!("{w}");
    }
    print!("{doc}");
}

pub(crate) async fn docs_create(slug: &str, stdin: bool, md: &Option<String>) -> Result<()> {
    let content = body_input(stdin, md)?;
    let DocWrite { doc, warnings } = doc_store().create_doc(slug, &content).await?;
    print_write(&doc, &warnings);
    Ok(())
}

pub(crate) async fn docs_append(
    slug: &str,
    stdin: bool,
    text: &[String],
    force: bool,
) -> Result<()> {
    let store = doc_store();
    let addition = if stdin {
        body_input(true, &None)?
    } else {
        let joined = text.join(" ").trim().to_string();
        if joined.is_empty() {
            bail!("вход пуст: --stdin или текст аргументом");
        }
        joined
    };
    if addition.trim().is_empty() {
        bail!("вход пуст — тихий no-op замаскирует сломанный пайп вызывающего");
    }
    let doc = match store.exists(slug).await? {
        true => store.append_doc(slug, &addition).await?,
        false if force => store.create_doc(slug, &addition).await?.doc,
        false => bail!("док «{slug}» не найден — создай через docs create (или append --force)"),
    };
    print!("{doc}");
    Ok(())
}

pub(crate) async fn docs_replace(
    slug: &str,
    stdin: bool,
    md: &Option<String>,
    base: &Option<String>,
) -> Result<()> {
    let body = body_input(stdin, md)?;
    let doc = doc_store()
        .replace_doc(slug, &body, base.as_deref())
        .await?;
    print!("{doc}");
    Ok(())
}

pub(crate) async fn docs_delete(slug: &str) -> Result<()> {
    doc_store().delete_doc(slug).await?;
    println!("док снесён: {slug} (откат — git revert в uwuwu-hub)");
    Ok(())
}

pub(crate) async fn docs_tree() -> Result<()> {
    for slug in doc_store().tree().await? {
        println!("{slug}");
    }
    Ok(())
}

/// Search hub: полнотекст (rg/grep по клону) + заголовки (hpath по путям)
/// — две секции top-n, semantic — вниз (старая машина Ollama поверх
/// того же индекса, дельта из hub).
pub(crate) async fn search(query: &str, limit: usize, threshold: f32) -> Result<()> {
    let store = doc_store();
    let fulltext = store.search_fulltext(query, limit).await?;
    let titles = store.search_by_title(query, limit).await?;

    println!("Полнотекст (git-клон uwuwu-hub):");
    if fulltext.is_empty() {
        println!("  — пусто");
    }
    for h in &fulltext {
        println!("  — {}: {}", h.title, h.snippet);
    }
    println!("Доки по заголовкам:");
    if titles.is_empty() {
        println!("  — пусто");
    }
    for t in &titles {
        println!("  — {}", t.slug);
    }
    // Semantic-секция: индекс per-машинный; дельта в остатке бюджета —
    // hub-источник (semantic::search_half с None-sy). Деградация секции —
    // честный статус, команда не падает.
    let cfg = crate::semantic::SemanticConfig::from_env();
    let section = crate::semantic::search_half(&cfg, query, limit, threshold).await;
    println!();
    for line in section {
        println!("{line}");
    }
    Ok(())
}

#[cfg(test)]
mod docs_cmd_tests {
    use crate::store::doc_store::{DocRef, DocStore};
    use crate::store::github_doc_rig::{hub_env, PLAIN_DOC};
    use serial_test::serial;

    /// append --force создаёт док при отсутствии (ветка cmd-слоя, ревью
    /// Common-2: force-ветка не покрыта).
    #[tokio::test]
    #[serial]
    async fn append_force_creates_missing_doc() -> Result<(), anyhow::Error> {
        let tmp = tempfile::TempDir::new()?;
        let _env = hub_env(&tmp).await?;
        let missing = "Experience/theme/по-force";
        super::docs_append(missing, false, &["дельта через force".to_string()], true).await?;
        let md = super::doc_store()
            .read(&DocRef {
                slug: missing.into(),
            })
            .await?;
        assert!(md.contains("title: по-force"));
        assert!(md.contains("дельта через force"));
        Ok(())
    }

    /// append без --force на отсутствующем доке — громкий отказ (строгость
    /// exists-чека не подменяется force-веткой).
    #[tokio::test]
    #[serial]
    async fn append_without_force_on_missing_doc_rejects() -> Result<(), anyhow::Error> {
        let tmp = tempfile::TempDir::new()?;
        let _env = hub_env(&tmp).await?;
        let missing = "Experience/theme/нет-дока";
        let err = super::docs_append(missing, false, &["дельта".to_string()], false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("docs create"));
        Ok(())
    }

    /// Пустой ввод append — громкий отказ, не тихий no-op (ревью Low-1).
    #[tokio::test]
    #[serial]
    async fn append_empty_stdin_rejects() -> Result<(), anyhow::Error> {
        let tmp = tempfile::TempDir::new()?;
        let _env = hub_env(&tmp).await?;
        let err = super::docs_append(PLAIN_DOC, true, &[], false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("вход пуст"));
        Ok(())
    }
}
