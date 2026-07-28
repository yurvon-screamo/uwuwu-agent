#!/usr/bin/env bun
/**
 * bin.ts — neutral entry point for the atlassian CLI.
 *
 * Loads `.env` from the script directory (if present), validates required
 * env vars, and runs the bundled MCP server (`atlassian.ts`).
 *
 * Required env vars (see `.env.example`):
 *   CONFLUENCE_PERSONAL_TOKEN
 *   JIRA_PERSONAL_TOKEN
 *   CONFLUENCE_USERNAME
 *   JIRA_USERNAME
 *   CONFLUENCE_URL
 *   JIRA_URL
 */
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));

function loadDotEnv(envPath: string): void {
  if (!existsSync(envPath)) return;
  const content = readFileSync(envPath, "utf-8");
  for (const line of content.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;
    const eqIdx = trimmed.indexOf("=");
    if (eqIdx === -1) continue;
    const key = trimmed.slice(0, eqIdx).trim();
    const value = trimmed.slice(eqIdx + 1).trim();
    if (process.env[key] === undefined || process.env[key] === "") {
      process.env[key] = value;
    }
  }
}

const envPath = join(__dirname, ".env");
loadDotEnv(envPath);

const required = [
  "CONFLUENCE_PERSONAL_TOKEN",
  "JIRA_PERSONAL_TOKEN",
  "CONFLUENCE_USERNAME",
  "JIRA_USERNAME",
  "CONFLUENCE_URL",
  "JIRA_URL",
];
const missing = required.filter((k) => !process.env[k]);
if (missing.length > 0) {
  console.error(
    `atlassian: missing required env vars: ${missing.join(", ")}\n` +
      `Fill them in ${envPath} (see .env.example).\n` +
      `Process env (e.g. $env:CONFLUENCE_PERSONAL_TOKEN) overrides .env.`,
  );
  process.exit(1);
}

// atlassian.ts auto-runs its CLI on import when MCPORTER_DISABLE_AUTORUN !== "1".
await import("./atlassian.ts");
