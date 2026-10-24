//! Blob download (`uwuwu-cli blob <key>`) and doc screenshot
//! (`uwuwu-cli shot <slug>`) — both ride the AFFiNE UI session.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;

use crate::affine_search_server::ServerSearchConfig;

/// Download a workspace blob by key and return its bytes.
/// Route: GET {ui}/api/workspaces/{ws}/blobs/{key} with a sign-in cookie.
pub async fn download_blob(ss: &ServerSearchConfig, workspace: &str, key: &str) -> Result<Vec<u8>> {
    let http = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .context("http client")?;
    let resp = http
        .post(format!("{}/api/auth/sign-in", ss.ui_url))
        .json(&json!({"email": ss.email, "password": ss.password}))
        .send()
        .await
        .context("affine sign-in")?;
    if !resp.status().is_success() {
        bail!("affine sign-in failed: HTTP {}", resp.status());
    }
    let resp = http
        .get(format!(
            "{}/api/workspaces/{}/blobs/{}",
            ss.ui_url,
            workspace,
            urlencode(key)
        ))
        .send()
        .await
        .context("blob request")?;
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = resp.bytes().await.context("blob body")?;
    if !status.is_success() || content_type.starts_with("text/html") {
        bail!("blob '{key}' не скачан: HTTP {status} ({content_type}); проверь ключ");
    }
    Ok(bytes.to_vec())
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The playwright screenshot script, embedded and written to a temp file
/// at invocation. Resolves playwright from NODE_PATH, the default bun
/// global install, or UWUWU_PLAYWRIGHT_PATH.
pub const SHOT_SCRIPT: &str = r#"//
// affine-shot.mjs — sign in, open a doc, screenshot it.
import { createRequire } from 'node:module';

const candidates = [
  process.env.UWUWU_PLAYWRIGHT_PATH,
  'playwright', // resolved via NODE_PATH when set
  '/home/yurvon/.bun/install/global/node_modules/playwright',
].filter(Boolean);
let pw = null;
for (const c of candidates) {
  try { pw = createRequire(import.meta.url)(c); break; } catch (e) { /* next */ }
}
if (!pw) { console.error('playwright не найден: задай UWUWU_PLAYWRIGHT_PATH'); process.exit(2); }

const { UI, EMAIL, PASSWORD, WS, DOC, OUT } = process.env;
const ui = new URL(UI);
const signIn = await fetch(`${UI}/api/auth/sign-in`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ email: EMAIL, password: PASSWORD }),
});
if (!signIn.ok) { console.error(`sign-in HTTP ${signIn.status}`); process.exit(3); }
const raw = signIn.headers.getSetCookie?.() ?? [];
const cookies = raw.map((line) => {
  const [pair] = line.split(';');
  const eq = pair.indexOf('=');
  return {
    name: pair.slice(0, eq).trim(),
    value: pair.slice(eq + 1).trim(),
    domain: ui.hostname,
    path: '/',
  };
});

const browser = await pw.chromium.launch({ headless: true });
const context = await browser.newContext({
  viewport: { width: 1680, height: 1100 },
  deviceScaleFactor: 2,
});
await context.addCookies(cookies);
const page = await context.newPage();
const mode = process.env.MODE === 'edgeless' ? '?mode=edgeless' : '';
const url = `${UI}/workspace/${WS}/${DOC}${mode}`;
try {
  await page.goto(url, { waitUntil: 'domcontentloaded', timeout: 60_000 });
} catch (e) {
  console.error(`goto warning: ${e.message}`);
}
// Дождаться РЕАЛЬНОГО контента: сайдбар рендерит скелетоны первыми,
// док приходит через websocket-синк — это может занять десятки секунд.
try {
  await page.waitForSelector(
    '.affine-page-root-block-container, [data-block-id], affine-surface, .edgeless-container',
    { timeout: 45_000 }
  );
} catch (e) { console.error('content selector warning:', e.message); }
// Закрыть промо-диалог "Open this doc in AFFiNE app", если вылез
try {
  const dismiss = page.getByText('Dismiss', { exact: true });
  if (await dismiss.count()) await dismiss.click({ timeout: 2000 });
} catch (e) { /* нет диалога — ок */ }
// Дождаться исчезновения скелетонов (плейсхолдеры боковой панели)
try {
  await page.waitForFunction(
    () => !document.querySelector('.affine-page-root-block-container') ||
      document.querySelector('.affine-page-root-block-container')?.childElementCount > 0,
    { timeout: 20_000 }
  );
} catch (e) { /* не дождались — снимаем как есть */ }
await page.waitForTimeout(4000); // дать редактору дорисовать
await page.screenshot({ path: OUT, fullPage: true });
await browser.close();
console.log(OUT);
"#;

/// Run the screenshot flow for a resolved doc id; returns the output path.
/// `edgeless` opens the doc in edgeless mode (mindmaps, canvas elements).
pub async fn shot_doc(
    ss: &ServerSearchConfig,
    workspace: &str,
    doc_id: &str,
    out: &std::path::Path,
    edgeless: bool,
) -> Result<String> {
    let dir = std::env::temp_dir().join("uwuwu-shot");
    std::fs::create_dir_all(&dir)?;
    // salted name: predictable /tmp paths are a swap/DoS vector on
    // multi-user hosts
    let script = dir.join(format!("affine-shot-{}.mjs", std::process::id()));
    std::fs::write(&script, SHOT_SCRIPT)?;
    let out_str = out.to_string_lossy().to_string();
    let status = tokio::process::Command::new("node")
        .arg(&script)
        .env("UI", &ss.ui_url)
        .env("EMAIL", &ss.email)
        .env("PASSWORD", &ss.password)
        .env("WS", workspace)
        .env("DOC", doc_id)
        .env("MODE", if edgeless { "edgeless" } else { "page" })
        .env("OUT", &out_str)
        .status()
        .await
        .context("запуск node (нужен node в PATH)")?;
    if !status.success() {
        bail!("shot упал: node exit {status}");
    }
    Ok(out_str)
}

/// Attach local files to a doc: upload each as a workspace blob, then
/// append an image block (raster/vector images, with caption) or an
/// attachment block (everything else). Returns the number attached.
pub async fn attach_files(
    cfg: &crate::affine_client::AffineConfig,
    doc_id: &str,
    paths: &[std::path::PathBuf],
) -> Result<usize> {
    use base64::Engine as _;

    let mut client = crate::affine_client::AffineClient::connect(cfg).await?;
    let mut attached = 0usize;
    for path in paths {
        let bytes =
            std::fs::read(path).with_context(|| format!("не читается {}", path.display()))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "attachment".to_string());
        let mime = mime_for(&name);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let resp = client
            .call_tool(
                "upload_blob",
                json!({
                    "workspaceId": cfg.workspace,
                    "content": b64,
                    "encoding": "base64",
                    "filename": name,
                    "contentType": mime,
                }),
            )
            .await
            .with_context(|| format!("upload_blob {name}"))?;
        let key = ["key", "blobKey", "sourceId"]
            .iter()
            .find_map(|k| resp.get(*k).and_then(|v| v.as_str()))
            .ok_or_else(|| anyhow!("upload_blob {name}: нет ключа блоба: {resp:.120}"))?;
        let block = if mime.starts_with("image/") {
            json!({
                "workspaceId": cfg.workspace, "docId": doc_id,
                "type": "image", "sourceId": key, "caption": name,
            })
        } else {
            json!({
                "workspaceId": cfg.workspace, "docId": doc_id,
                "type": "attachment", "sourceId": key,
                "name": name, "mimeType": mime, "size": bytes.len(),
            })
        };
        client
            .call_tool("append_block", block)
            .await
            .with_context(|| format!("append_block {name}"))?;
        attached += 1;
        println!("attached: {name} ({mime}, {} bytes) → {key}", bytes.len());
    }
    Ok(attached)
}

fn mime_for(name: &str) -> String {
    let ext = name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "json" => "application/json",
        "csv" => "text/csv",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
    .to_string()
}
