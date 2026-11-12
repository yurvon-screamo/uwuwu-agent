//! read/blob/projects/doctor: чтение доков, блобы, перечень ноутбуков,
//! диагностика деградации ядра.

use crate::cmd::read_resolve;
use crate::md_artifacts;
use crate::siyuan::SiYuan;
use anyhow::Result;
use serde_json::Value;

/// Слаг → markdown дока (stdout печатает dispatch). Формы: `sofa/<post_id>`,
/// `<project>/<doc>`, `<topic>/<doc>`, слаг Daily, одиночный заголовок
/// (если однозначен) — резолв в `read_resolve`.
pub async fn read_markdown(slug: &str) -> Result<String> {
    if let Some(post_id) = slug.strip_prefix("sofa/") {
        let post = crate::sofa::get(post_id).await?;
        return Ok(format!("# {}\n\n{}\n", post.title, post.body));
    }
    let sy = SiYuan::from_env()?;
    let doc_id = read_resolve::resolve(&sy, slug).await?;
    let md = sy.export_md_by_id(&doc_id).await?;
    for warning in md_artifacts::artifact_report(&md) {
        eprintln!("⚠ round-trip-артефакт: {warning}");
    }
    Ok(md)
}

pub(crate) async fn blob_run(key: &str, out: Option<String>) -> Result<()> {
    let sy = SiYuan::from_env()?;
    let path = format!("assets/{key}");
    let bytes = sy.download_file(&path).await?;
    let name = out.unwrap_or_else(|| key.to_string());
    if name == "-" {
        use std::io::Write;
        std::io::stdout().write_all(&bytes)?;
    } else {
        std::fs::write(&name, &bytes)?;
        println!("{key} → {} bytes → {}", bytes.len(), name);
    }
    Ok(())
}

pub(crate) async fn projects_run() -> Result<()> {
    let sy = SiYuan::from_env()?;
    for nb in sy.notebooks().await? {
        if nb.get("closed").and_then(|c| c.as_bool()).unwrap_or(false) {
            continue;
        }
        println!("{}", nb.get("name").and_then(|n| n.as_str()).unwrap_or(""));
    }
    Ok(())
}

// ——— kernel: живые пробы ядра (one-shot, без ретраев) ———

/// Имена записей readDir: директории с хвостовым «/».
pub(crate) fn ls_lines(entries: &[Value]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|entry| {
            entry.get("name").and_then(|n| n.as_str()).map(|name| {
                if entry
                    .get("isDir")
                    .and_then(|d| d.as_bool())
                    .unwrap_or(false)
                {
                    format!("{name}/")
                } else {
                    name.to_string()
                }
            })
        })
        .collect()
}

/// doctor: read-only пробы деградации; свой exit-код поверх anyhow-механики
/// (0 чисто / 1 деградация / 2 пробы не выполнились) — консьюмеры exit-кода
/// (кроны, вотчдог) различают исходы.
pub(super) async fn doctor_run(project: &str, since_min: u64) -> Result<()> {
    let sy = crate::siyuan::SiYuan::from_env_with_timeout(10)?;
    let outcomes = crate::siyuan_doctor::run(&sy, project, since_min).await;
    println!("{}", crate::siyuan_doctor::report(&outcomes));
    std::process::exit(crate::siyuan_doctor::doctor_exit_code(&outcomes));
}
