//! Blob download (`uwuwu-cli blob <key>`) and doc screenshot
//! (`uwuwu-cli shot <slug>`) — both ride the AFFiNE UI session.

use anyhow::{bail, Context, Result};
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
    let script = dir.join("affine-shot.mjs");
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
