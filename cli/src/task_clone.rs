use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::file_io::collect_normal_components;
use crate::path as safe_path;

pub fn clone_task(
    project: &str,
    slug: &str,
    dest: Option<&str>,
    force: bool,
    wiki_root: &Path,
) -> Result<PathBuf> {
    let src_dir = safe_path::resolve_task_dir(project, slug, wiki_root)?;

    let dest_dir = resolve_dest(slug, dest)?;

    if dest_dir.exists() {
        if !force {
            anyhow::bail!(
                "destination already exists: {} (use --force to overwrite)",
                dest_dir.display()
            );
        }
        std::fs::remove_dir_all(&dest_dir)
            .with_context(|| format!("cannot remove existing dest: {}", dest_dir.display()))?;
    }

    std::fs::create_dir_all(&dest_dir)
        .with_context(|| format!("cannot create dest: {}", dest_dir.display()))?;

    let mut copied = Vec::new();
    copy_dir_recursive(&src_dir, &dest_dir, &mut copied)?;

    println!(
        "Cloned {}/{} → {} ({} files)",
        project,
        slug,
        dest_dir.display(),
        copied.len()
    );
    for f in &copied {
        println!("  {}", f.display());
    }

    Ok(dest_dir)
}

pub fn handle_clone(
    project: &str,
    slug: &str,
    dest: Option<&str>,
    force: bool,
    wiki_root: &Path,
) -> Result<()> {
    clone_task(project, slug, dest, force, wiki_root)?;
    Ok(())
}

fn resolve_dest(slug: &str, dest: Option<&str>) -> Result<PathBuf> {
    if let Some(custom) = dest {
        return resolve_dest_via_env_or_custom(slug, Some(custom));
    }
    resolve_dest_via_env_or_custom(slug, None)
}

fn resolve_dest_via_env_or_custom(slug: &str, custom_dest: Option<&str>) -> Result<PathBuf> {
    if let Some(env_tasks_dir) = std::env::var("UWUWU_TASKS_DIR")
        .ok()
        .filter(|s| !s.is_empty())
    {
        let base = PathBuf::from(env_tasks_dir);
        return Ok(base.join(slug));
    }

    let cwd = std::env::current_dir().context("cannot get cwd")?;
    let cwd_canonical = cwd
        .canonicalize()
        .with_context(|| format!("cannot canonicalize cwd: {}", cwd.display()))?;

    let dest_base = match custom_dest {
        Some(c) => {
            let components = collect_normal_components(c)?;
            let mut path = cwd_canonical.clone();
            for comp in &components {
                path = path.join(comp);
            }
            path
        }
        None => cwd_canonical
            .join(".uwuwu-workspace")
            .join("tasks")
            .join(slug),
    };

    if !dest_base.starts_with(&cwd_canonical) {
        anyhow::bail!("destination escapes cwd: {}", dest_base.display());
    }

    Ok(dest_base)
}

fn copy_dir_recursive(src: &Path, dst: &Path, copied: &mut Vec<PathBuf>) -> Result<()> {
    let entries =
        std::fs::read_dir(src).with_context(|| format!("cannot read dir: {}", src.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let target = dst.join(&name);

        if path.is_dir() {
            std::fs::create_dir_all(&target)
                .with_context(|| format!("cannot create dir: {}", target.display()))?;
            copy_dir_recursive(&path, &target, copied)?;
        } else {
            std::fs::copy(&path, &target).with_context(|| {
                format!("cannot copy {} → {}", path.display(), target.display())
            })?;
            copied.push(target);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;

    fn seed_task(root: &Path, project: &str, slug: &str, body: &str) {
        let dir = root.join("projects").join(project).join("tasks").join(slug);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("README.md"), body).unwrap();
    }

    fn seed_asset(root: &Path, project: &str, slug: &str, asset: &str, content: &[u8]) {
        let dir = root.join("projects").join(project).join("tasks").join(slug);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(asset), content).unwrap();
    }

    #[test]
    #[serial(env_tasks)]
    fn clone_task_copies_readme_and_assets_via_env() {
        let tmp = tempfile::tempdir().unwrap();
        seed_task(tmp.path(), "ckii", "t1", "readme body");
        seed_asset(tmp.path(), "ckii", "t1", "screenshot.png", b"PNGDATA");

        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        std::env::set_var("UWUWU_TASKS_DIR", &workspace);

        let result = clone_task("ckii", "t1", None, false, tmp.path());
        std::env::remove_var("UWUWU_TASKS_DIR");
        let target = result.unwrap();
        assert!(target.join("README.md").exists());
        assert!(target.join("screenshot.png").exists());
    }

    #[test]
    #[serial(env_tasks)]
    fn clone_task_refuses_without_force_when_dest_exists() {
        let tmp = tempfile::tempdir().unwrap();
        seed_task(tmp.path(), "ckii", "t1", "body");

        let workspace = tmp.path().join("workspace");
        let target = workspace.join("t1");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("README.md"), "old").unwrap();
        std::env::set_var("UWUWU_TASKS_DIR", &workspace);

        let err = clone_task("ckii", "t1", None, false, tmp.path());
        std::env::remove_var("UWUWU_TASKS_DIR");
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("--force") || msg.contains("already exists"));
    }

    #[test]
    #[serial(env_tasks)]
    fn clone_task_with_force_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        seed_task(tmp.path(), "ckii", "t1", "new body");

        let workspace = tmp.path().join("workspace");
        let target = workspace.join("t1");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("README.md"), "old").unwrap();
        std::env::set_var("UWUWU_TASKS_DIR", &workspace);

        clone_task("ckii", "t1", None, true, tmp.path()).unwrap();
        std::env::remove_var("UWUWU_TASKS_DIR");

        let content = fs::read_to_string(target.join("README.md")).unwrap();
        assert_eq!(content, "new body");
    }

    #[test]
    #[serial(env_tasks)]
    fn clone_task_rejects_path_escape_in_dest() {
        let tmp = tempfile::tempdir().unwrap();
        seed_task(tmp.path(), "ckii", "t1", "body");

        std::env::remove_var("UWUWU_TASKS_DIR");
        let _ = std::env::set_current_dir(tmp.path());
        let err = clone_task("ckii", "t1", Some("../escape"), false, tmp.path());
        assert!(err.is_err());
    }

    #[test]
    #[serial(env_tasks)]
    fn clone_task_handles_cyrillic_asset_filename() {
        let tmp = tempfile::tempdir().unwrap();
        seed_task(tmp.path(), "ckii", "t1", "body");
        seed_asset(tmp.path(), "ckii", "t1", "тест.pdf", b"binary");

        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        std::env::set_var("UWUWU_TASKS_DIR", &workspace);

        let target = clone_task("ckii", "t1", None, false, tmp.path()).unwrap();
        std::env::remove_var("UWUWU_TASKS_DIR");
        assert!(target.join("тест.pdf").exists());
    }
}
