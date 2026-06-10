---
name: tool-integration-browser
description: "Browser automation via Playwriter CLI. Control user's Edge browser: navigate, click, type, extract data, run JS. Triggers: 'open browser', 'click button', 'fill form', 'scrape page', 'take screenshot', 'browser automation', 'playwriter'. Use when any browser interaction is needed."
---

# CLI: Browser (`playwriter`)

Browser automation via the `playwriter` CLI tool. Controls the user's **Edge** browser (`yurvon@yandex.ru` profile) — all sites are already logged in.

## REQUIRED: Read Full Documentation First

**Before any session, run:**

```bash
playwriter skill # IMPORTANT! Read in FULL. Do NOT pipe through head/tail.
```

This outputs complete documentation covering session management, timeout config, selector strategies, common pitfalls, context variables, and utility functions. **Do NOT skip this step.**

## Sessions

```bash
playwriter browser list                    # List available browsers
playwriter session new                     # Create new session (opens tab)
playwriter session list                    # List active sessions
playwriter session close -s <SESSION_ID>   # Close session
```

**Reuse existing sessions** to avoid opening extra browser tabs. Only run `playwriter session new` if no session exists.

## Browser Profile

Use Edge with the user profile:

```bash
playwriter browser list
# Example output:
# KEY                       TYPE       BROWSER  PROFILE
# ------------------------------------------------------------
# profile:c6d9854de99ac36d  extension  Edge     yurvon@yandex.ru
#
# Use with: playwriter session new [--browser <key>]
```

## Executing JavaScript

```bash
playwriter -s <SESSION_ID> -e '<JS_CODE>'
```

**ALWAYS use single quotes** (`'...'`) for the `-e` argument. Single quotes prevent the shell from interpreting `$`, backticks, and backslashes. Inside JS strings, use **double quotes** (`"..."`) or **backtick template literals** (`` `...` ``).

**NEVER** use heredoc (`<<'EOF'`) or `$(cat ...)` constructs — pass JS code directly via `-e`.

## Navigation

```bash
playwriter -s 1 -e 'await page.goto("https://example.com")'
playwriter -s 1 -e 'const url = page.url(); console.log(url)'
playwriter -s 1 -e 'await page.goBack()'
playwriter -s 1 -e 'await page.goForward()'
playwriter -s 1 -e 'await page.reload()'
```

## Selectors & Interaction

```bash
# Click
playwriter -s 1 -e 'await page.click("button.submit")'

# Type text
playwriter -s 1 -e 'await page.fill("input#email", "user@example.com")'

# Press key
playwriter -s 1 -e 'await page.press("input#search", "Enter")'

# Select option
playwriter -s 1 -e 'await page.selectOption("select#lang", "en")'

# Upload file
playwriter -s 1 -e 'await page.setInputFiles("input[type=file]", "/path/to/file.pdf")'
```

## Extracting Data

```bash
# Get text content
playwriter -s 1 -e 'const text = await page.textContent("h1"); console.log(text)'

# Get all matching elements
playwriter -s 1 -e 'const items = await page.$$eval(".item", els => els.map(e => e.textContent)); console.log(JSON.stringify(items))'

# Get attribute
playwriter -s 1 -e 'const href = await page.getAttribute("a.link", "href"); console.log(href)'

# Evaluate arbitrary JS
playwriter -s 1 -e 'const title = await page.evaluate(() => document.title); console.log(title)'
```

## Waiting

```bash
# Wait for selector
playwriter -s 1 -e 'await page.waitForSelector(".loaded", {timeout: 10000})'

# Wait for navigation
playwriter -s 1 -e 'await page.waitForURL("**/dashboard**")'

# Wait for timeout
playwriter -s 1 -e 'await page.waitForTimeout(3000)'

# Wait for load state
playwriter -s 1 -e 'await page.waitForLoadState("networkidle")'
```

## Screenshots

```bash
playwriter -s 1 -e 'await page.screenshot({path: "screenshot.png", fullPage: true})'
```

## Multi-step Scripts

For complex operations, chain await calls in a single `-e` invocation:

```bash
playwriter -s 1 -e 'await page.goto("https://example.com/login"); await page.fill("#username", "user"); await page.fill("#password", "pass"); await page.click("button[type=submit]"); await page.waitForURL("**/dashboard**"); console.log("Login done:", page.url())'
```

## Operational Protocol

1. **Read docs**: Run `playwriter skill` first if not already read.
2. **Check session**: Run `playwriter session list` — reuse existing session if available.
3. **Analyze**: Understand what needs to happen on the page.
4. **Execute**: Build and run the `playwriter` command.
5. **Report**: Provide a brief execution result.

Report format:

```
📊 ОТЧЁТ О ВЫПОЛНЕНИИ
═══════════════════════════════════════
🎯 Задача: [Описание]
✅/❌ Статус: [УСПЕХ / ОШИБКА]
───────────────────────────────────────
📝 Действия:
  • [Шаг 1]
  • [Шаг 2]
───────────────────────────────────────
📋 Результат:
  [Краткие данные или выводы]
───────────────────────────────────────
⚠️ Ошибки: [Если есть]
═══════════════════════════════════════
```

## Rules

- ALWAYS run `playwriter skill` and read the full output before first use in a session
- ALWAYS reuse existing browser sessions — check with `playwriter session list` first
- ALWAYS use single quotes for `-e` arguments; double quotes or backticks for JS strings
- NEVER use heredoc (`<<'EOF'`) or `$(cat ...)` constructs
- ALWAYS add `await` before Playwright async methods (`page.goto`, `page.click`, etc.)
- ALWAYS handle potential timeouts with `waitForSelector` or `waitForLoadState` for slow pages
- Use Edge browser with the `yurvon@yandex.ru` profile — sites are already logged in
- Prefer `page.fill` over `page.type` for form fields (clears existing value)
- Use `console.log()` to return data from `-e` commands
