use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};

use crate::path as safe_path;
use base64::Engine;

pub const MAX_IMAGE_ASSET_BYTES: u64 = 5 * 1024 * 1024;

pub struct AssetContent {
    pub kind: AssetKind,
    pub source_path: PathBuf,
}

pub enum AssetKind {
    Text(String),
    Image { base64: String, mime: String },
    BinaryUnsupported { size_bytes: u64 },
    TooLarge { size_bytes: u64 },
}

pub fn read_asset(
    project: &str,
    slug: &str,
    asset_name: &str,
    wiki_root: &Path,
) -> Result<AssetContent> {
    let task_dir = safe_path::resolve_task_dir(project, slug, wiki_root)?;
    let asset_path = resolve_asset_path(&task_dir, asset_name)?;

    let ext = asset_path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    if is_text_ext(&ext) {
        let text = std::fs::read_to_string(&asset_path)
            .with_context(|| format!("cannot read asset: {}", asset_path.display()))?;
        return Ok(AssetContent {
            kind: AssetKind::Text(text),
            source_path: asset_path,
        });
    }

    if is_image_ext(&ext) {
        let metadata = std::fs::metadata(&asset_path)
            .with_context(|| format!("cannot stat: {}", asset_path.display()))?;
        if metadata.len() > MAX_IMAGE_ASSET_BYTES {
            return Ok(AssetContent {
                kind: AssetKind::TooLarge {
                    size_bytes: metadata.len(),
                },
                source_path: asset_path,
            });
        }
        let bytes = std::fs::read(&asset_path)
            .with_context(|| format!("cannot read: {}", asset_path.display()))?;
        let mime = mime_for_ext(&ext);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        return Ok(AssetContent {
            kind: AssetKind::Image { base64: b64, mime },
            source_path: asset_path,
        });
    }

    let metadata = std::fs::metadata(&asset_path)
        .with_context(|| format!("cannot stat: {}", asset_path.display()))?;
    Ok(AssetContent {
        kind: AssetKind::BinaryUnsupported {
            size_bytes: metadata.len(),
        },
        source_path: asset_path,
    })
}

pub fn handle_asset_get(
    project: &str,
    slug: &str,
    asset_name: &str,
    wiki_root: &Path,
) -> Result<()> {
    let asset = read_asset(project, slug, asset_name, wiki_root)?;
    match asset.kind {
        AssetKind::Text(t) => {
            print!("{t}");
            Ok(())
        }
        AssetKind::Image { .. } => {
            eprintln!(
                "Image asset: {} ({} bytes).",
                asset.source_path.display(),
                std::fs::metadata(&asset.source_path)
                    .map(|m| m.len())
                    .unwrap_or(0)
            );
            eprintln!("Binary image content not displayed in CLI; use `task clone` or MCP `task_asset_get`.");
            Ok(())
        }
        AssetKind::BinaryUnsupported { size_bytes } => {
            eprintln!(
                "Binary asset '{}' not supported via CLI ({} bytes). Use `task clone` to obtain locally.",
                asset_name, size_bytes
            );
            Ok(())
        }
        AssetKind::TooLarge { size_bytes } => {
            eprintln!(
                "Asset '{}' too large ({} bytes > {} bytes). Use `task clone` to obtain locally.",
                asset_name, size_bytes, MAX_IMAGE_ASSET_BYTES
            );
            Ok(())
        }
    }
}

pub fn list_assets(project: &str, slug: &str, wiki_root: &Path) -> Result<Vec<AssetInfo>> {
    let task_dir = safe_path::resolve_task_dir(project, slug, wiki_root)?;
    let mut assets = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&task_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "README.md" {
                continue;
            }
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let kind = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            assets.push(AssetInfo {
                name,
                size_bytes: size,
                ext: kind,
            });
        }
    }
    assets.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(assets)
}

pub struct AssetInfo {
    pub name: String,
    pub size_bytes: u64,
    pub ext: String,
}

fn resolve_asset_path(task_dir: &Path, asset_name: &str) -> Result<PathBuf> {
    let parsed = Path::new(asset_name);
    if parsed.is_absolute() {
        anyhow::bail!("asset name must be relative: {asset_name}");
    }
    for component in parsed.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir => anyhow::bail!("asset name must not contain '.': {asset_name}"),
            Component::ParentDir => anyhow::bail!("asset name must not contain '..': {asset_name}"),
            Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("asset name must be relative (no root/prefix): {asset_name}")
            }
        }
    }

    let candidate = task_dir.join(asset_name);
    if !candidate.exists() {
        anyhow::bail!("asset not found: {asset_name}");
    }

    let canonical_task_dir = task_dir
        .canonicalize()
        .with_context(|| format!("cannot canonicalize task dir: {}", task_dir.display()))?;
    let canonical_candidate = candidate
        .canonicalize()
        .with_context(|| format!("cannot canonicalize: {}", candidate.display()))?;
    if !canonical_candidate.starts_with(&canonical_task_dir) {
        anyhow::bail!("asset path escapes task dir: {asset_name}");
    }

    Ok(canonical_candidate)
}

fn is_text_ext(ext: &str) -> bool {
    matches!(
        ext,
        "md" | "txt" | "json" | "csv" | "log" | "yaml" | "yml" | "toml"
    )
}

fn is_image_ext(ext: &str) -> bool {
    matches!(ext, "png" | "jpg" | "jpeg" | "gif" | "webp")
}

fn mime_for_ext(ext: &str) -> String {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn seed_task_with_asset(root: &Path, project: &str, slug: &str, asset: &str, content: &[u8]) {
        let dir = root.join("projects").join(project).join("tasks").join(slug);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("README.md"), "task body").unwrap();
        fs::write(dir.join(asset), content).unwrap();
    }

    #[test]
    fn read_asset_returns_text_for_md() {
        let tmp = tempfile::tempdir().unwrap();
        seed_task_with_asset(tmp.path(), "ckii", "t1", "notes.md", b"# Notes\n\ntext");

        let asset = read_asset("ckii", "t1", "notes.md", tmp.path()).unwrap();
        match asset.kind {
            AssetKind::Text(t) => assert!(t.contains("Notes")),
            _ => panic!("expected Text kind"),
        }
    }

    #[test]
    fn read_asset_returns_image_for_png() {
        let tmp = tempfile::tempdir().unwrap();
        seed_task_with_asset(tmp.path(), "ckii", "t1", "screenshot.png", b"PNGDATA");

        let asset = read_asset("ckii", "t1", "screenshot.png", tmp.path()).unwrap();
        match asset.kind {
            AssetKind::Image { base64, mime } => {
                assert_eq!(mime, "image/png");
                assert!(!base64.is_empty());
            }
            _ => panic!("expected Image kind"),
        }
    }

    #[test]
    fn read_asset_jpg_uppercase_ext_maps_to_image_jpeg() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("projects/ckii/tasks/t1");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("README.md"), "body").unwrap();
        fs::write(dir.join("photo.JPG"), b"JPGDATA").unwrap();

        let asset = read_asset("ckii", "t1", "photo.JPG", tmp.path()).unwrap();
        match asset.kind {
            AssetKind::Image { mime, .. } => assert_eq!(mime, "image/jpeg"),
            _ => panic!("expected Image kind"),
        }
    }

    #[test]
    fn read_asset_rejects_path_escape_in_asset_name() {
        let tmp = tempfile::tempdir().unwrap();
        seed_task_with_asset(tmp.path(), "ckii", "t1", "notes.md", b"text");

        let err = read_asset("ckii", "t1", "../../../etc/passwd", tmp.path());
        assert!(err.is_err());
    }

    #[test]
    fn read_asset_returns_binary_unsupported_for_pdf() {
        let tmp = tempfile::tempdir().unwrap();
        seed_task_with_asset(tmp.path(), "ckii", "t1", "doc.pdf", b"PDFBIN");

        let asset = read_asset("ckii", "t1", "doc.pdf", tmp.path()).unwrap();
        match asset.kind {
            AssetKind::BinaryUnsupported { .. } => {}
            _ => panic!("expected BinaryUnsupported"),
        }
    }

    #[test]
    fn read_asset_returns_too_large_for_oversized_image() {
        let tmp = tempfile::tempdir().unwrap();
        let big_content = vec![0u8; (MAX_IMAGE_ASSET_BYTES + 1) as usize];
        seed_task_with_asset(tmp.path(), "ckii", "t1", "big.png", &big_content);

        let asset = read_asset("ckii", "t1", "big.png", tmp.path()).unwrap();
        match asset.kind {
            AssetKind::TooLarge { .. } => {}
            _ => panic!("expected TooLarge"),
        }
    }

    #[test]
    fn list_assets_excludes_readme() {
        let tmp = tempfile::tempdir().unwrap();
        seed_task_with_asset(tmp.path(), "ckii", "t1", "notes.md", b"x");
        seed_task_with_asset(tmp.path(), "ckii", "t1", "screenshot.png", b"png");

        let assets = list_assets("ckii", "t1", tmp.path()).unwrap();
        assert_eq!(assets.len(), 2);
        assert!(assets.iter().all(|a| a.name != "README.md"));
    }
}
