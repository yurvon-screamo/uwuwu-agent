//! lib/hub-git.ts — git-чтение wiki для TS-демонов: дерево origin/main без
//! worktree. Демоны — читатели: worktree клона мутирует единственный писатель
//! (cli, свой ensure_clone с fetch+reset), поэтому демонские чтения идут через
//! `git show/ls-tree/cat-file` по origin/main — с его записями не пересекаются
//! по построению (инцидент 13–14.11, задача 1075: демоны читали worktree,
//! который свежеет только от cli doc-op'ов; стухший клон читался как
//! «пиннинг-док не найден» и сажал три демона в crash-loop).
//!
//! Свежесть: кэш-гейт `fetch --depth 1` (дефолт раз в 10 мин, env
//! UWUWU_HUB_CLONE_REFRESH_SEC; отрицательный — без сети, юнит-тесты; 0 —
//! каждый вызов). Неудачный fetch не молотит сеть каждым вызовом — ретрай
//! через интервал, чтения продолжаются по последнему скачанному дереву.
//!
//! Корень клона считается локально (base + сегмент репо), НЕ через
//! platform-hub.hubCloneRoot: bun test — общий процесс, mock.module соседних
//! тестов подменяет экспорты platform-hub на весь рантайм — низкоуровневый
//! git-читатель не должен наследовать этот фейл-домен (дельта-прогон полного
//! сьюта, задача 1075). Env читается на вызов: порядок импортов тестовых
//! файлов не должен замораживать конфиг (модуль импортируется первым
//! попавшим файлом сьюта).

import { spawnSync } from "node:child_process"

function cloneRoot(): string {
  const repo = process.env.UWUWU_HUB_REPO ?? ""
  const base = process.env.UWUWU_HUB_CLONE_ROOT
    ?? `${process.env.HOME ?? "."}/.local/share/uwuwu-cli/hub-clone`
  return `${base}/${repo.replace("/", "__")}`
}

function refreshSec(): number {
  return Number(process.env.UWUWU_HUB_CLONE_REFRESH_SEC ?? 600)
}

let lastFetchAt = 0

/** Сброс кэш-гейта fetch (юнит-тесты). */
export function resetHubGitCacheForTests(): void {
  lastFetchAt = 0
}

function git(args: string[]): { ok: boolean; stdout: string; stderr: string } {
  const r = spawnSync("git", ["-C", cloneRoot(), ...args], {
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
    timeout: 30_000,
  })
  return { ok: r.status === 0, stdout: r.stdout, stderr: r.stderr }
}

/** Подтянуть origin, если кэш протух. Отказ не роняет вызов: чтение идёт по
 *  последнему скачанному дереву (честная деградация — то же дерево, что
 *  читали до инцидента, только осознанно). */
function hubGitEnsureFresh(log?: (m: string) => void): void {
  if (refreshSec() < 0) return
  const now = Date.now()
  if (now - lastFetchAt < refreshSec() * 1000) return
  lastFetchAt = now // и при неуспехе: оффлайн не молотит каждый вызов — ретрай через интервал
  const r = git(["fetch", "--depth", "1", "origin"])
  if (!r.ok) {
    log?.(`hub-git: fetch не удался — читаю последнее скачанное дерево origin/main (${r.stderr.trim().slice(0, 120)})`)
  }
}

/** Файл из дерева origin/main; null — нет файла/дерева (вызывающий решает:
 *  обычно это exists:false контракта, не сбой). */
export function hubGitShow(relPath: string, log?: (m: string) => void): string | null {
  hubGitEnsureFresh(log)
  const r = git(["show", `origin/main:${relPath.replace(/^\/+/, "")}`])
  return r.ok ? r.stdout : null
}

/** Существование файла/папки в дереве origin/main. */
export function hubGitExists(relPath: string, log?: (m: string) => void): boolean {
  hubGitEnsureFresh(log)
  return git(["cat-file", "-e", `origin/main:${relPath.replace(/^\/+/, "")}`]).ok
}

export interface HubGitEntry {
  name: string
  isDir: boolean
}

/** Дети папки из дерева origin/main (паритет readdir: имя + папка ли).
 *  Пусто — папки нет/недоступна. `-z`: имена без quotePath-экранирования
 *  (вики-тайтлы кириллицей/пробелами — норма), NUL-разделитель. */
export function hubGitListDir(relDir: string, log?: (m: string) => void): HubGitEntry[] {
  hubGitEnsureFresh(log)
  // tree-ish colon-форма (без pathspec): дети с голыми именами; -z — без
  // quotePath-экранирования (кириллица/пробелы вики-тайтлов — норма)
  const dir = relDir.replace(/^\/+/, "").replace(/\/*$/, "")
  const r = git(["ls-tree", "-z", `origin/main:${dir}`])
  if (!r.ok) return []
  const out: HubGitEntry[] = []
  for (const line of r.stdout.split("\0")) {
    if (line.trim() === "") continue
    const tab = line.indexOf("\t")
    if (tab === -1) continue
    const meta = line.slice(0, tab)
    const name = line.slice(tab + 1)
    if (meta === "" || name === "") continue
    out.push({ name, isDir: meta.split(" ")[1] === "tree" })
  }
  return out
}
