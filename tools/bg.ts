import { tool } from "@opencode-ai/plugin";
import { mkdirSync, mkdtempSync, writeFileSync } from "fs";
import { homedir, platform } from "os";
import { join } from "path";
import { spawnSync } from "child_process";

const IS_WIN = platform() === "win32";
const BG_DIR = join(homedir(), ".opencode-bg");

// How long the launcher waits after spawning the window to collect early
// failures (chdir errors, cmdlet-not-found, etc.). Trade-off: long enough to
// catch "instant" crashes, short enough not to stall successful launches.
const EARLY_CHECK_MS = 1500;

function psQuote(s: string): string {
    // PowerShell single-quoted string escape: ' -> ''
    return s.replace(/'/g, "''");
}

function shQuote(s: string): string {
    // POSIX single-quoted string escape: ' -> '\''
    return s.replace(/'/g, "'\\''");
}

function failWindows(message: string): never {
    throw new Error(message);
}

function startWindows(command: string, cwd: string): object {
    mkdirSync(BG_DIR, { recursive: true });
    const dir = mkdtempSync(join(BG_DIR, "session-"));
    const runPath = join(dir, "run.ps1");
    const launcherPath = join(dir, "launch.ps1");
    const errPath = join(dir, "err.log");

    // Window script: chdir then run the command. Any early failure is written
    // to err.log so the launcher can surface it synchronously to the agent.
    const runScript = [
        "$ErrorActionPreference = 'Stop'",
        "try {",
        `    Set-Location -LiteralPath '${psQuote(cwd)}'`,
        "} catch {",
        `    \"chdir: $($_.Exception.Message)\" | Out-File -FilePath '${psQuote(errPath)}' -Encoding utf8`,
        "    throw",
        "}",
        "try {",
        "    & {",
        command,
        "    }",
        "} catch {",
        `    \"runtime: $($_.Exception.Message)\" | Out-File -FilePath '${psQuote(errPath)}' -Encoding utf8`,
        "    Write-Host ''",
        '    Write-Host "ERROR: $($_.Exception.Message)"',
        "    Write-Host '(window kept open — close manually)'",
        "}",
    ].join("\r\n");
    writeFileSync(runPath, runScript, "utf-8");

    // Launcher: parse-check the run script, spawn the window, wait briefly for
    // early errors, then either surface the error or emit the PID.
    // exit 2 = parse error (e.g. '&&' on Windows PowerShell), exit 3 = runtime.
    const launcherScript = [
        "$ErrorActionPreference = 'Stop'",
        `Remove-Item -LiteralPath '${psQuote(errPath)}' -ErrorAction SilentlyContinue`,
        `$cmd = Get-Content -Raw -LiteralPath '${psQuote(runPath)}'`,
        "try {",
        "    $null = [scriptblock]::Create($cmd)",
        "} catch {",
        '    [Console]::Error.WriteLine("parse: $($_.Exception.Message)")',
        "    exit 2",
        "}",
        "$p = Start-Process powershell.exe " +
            "-ArgumentList @('-NoExit','-NoProfile'," +
            "'-ExecutionPolicy','Bypass','-File'," +
            `'${psQuote(runPath)}') ` +
            `-WorkingDirectory '${psQuote(cwd)}' -WindowStyle Normal -PassThru`,
        `Start-Sleep -Milliseconds ${EARLY_CHECK_MS}`,
        `if (Test-Path -LiteralPath '${psQuote(errPath)}') {`,
        `    [Console]::Error.WriteLine((Get-Content -Raw -LiteralPath '${psQuote(errPath)}'))`,
        "    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue",
        "    exit 3",
        "}",
        "Write-Output $p.Id",
    ].join("\r\n");
    writeFileSync(launcherPath, launcherScript, "utf-8");

    const r = spawnSync(
        "powershell.exe",
        ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", launcherPath],
        { encoding: "utf-8", timeout: EARLY_CHECK_MS + 15000 },
    );

    if (r.status !== 0) {
        const detail = (r.stderr || "").trim() || `exit code ${r.status}`;
        failWindows(`background command failed to start:
${detail}`);
    }
    const pid = Number.parseInt((r.stdout || "").trim(), 10);
    if (!Number.isFinite(pid)) {
        failWindows(
            "failed to start background window:\n" +
                `stdout: ${(r.stdout || "").trim()}\n` +
                `stderr: ${(r.stderr || "").trim()}\n` +
                `status: ${r.status}`,
        );
    }
    return { ok: true, pid, cwd, window: true };
}

function startUnix(command: string, cwd: string): object {
    const r = spawnSync(
        "sh",
        [
            "-c",
            `cd '${shQuote(cwd)}' 2>/dev/null; nohup ${command} >/dev/null 2>&1 & echo $!`,
        ],
        { encoding: "utf-8" },
    );
    const pid = Number.parseInt((r.stdout || "").trim(), 10);
    return { ok: true, pid, cwd, window: false };
}

export const start = tool({
    description:
        "Run a command in a NEW background window and return its PID. " +
        "On Windows it opens a visible PowerShell window (non-blocking). " +
        "Use this for long-running commands (servers, dev servers, watchers) " +
        "instead of bash so the agent session is not blocked. If the command " +
        "fails immediately (syntax error, bad directory, command not found) the " +
        "error is returned to the caller instead of the PID. On Unix it falls " +
        "back to a detached nohup process (no window).",
    args: {
        command: tool.schema
            .string()
            .describe(
                "Shell command to run in a new background window, e.g. 'npm run dev'",
            ),
        cwd: tool.schema
            .string()
            .optional()
            .describe("Working directory (defaults to current process cwd)"),
    },
    async execute(args) {
        const cwd = args.cwd?.trim() || process.cwd();
        const result = IS_WIN
            ? startWindows(args.command, cwd)
            : startUnix(args.command, cwd);
        return JSON.stringify(result, null, 2);
    },
});
