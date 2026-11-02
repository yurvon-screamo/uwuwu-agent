// single-instance-child.ts — спавн-ребёнок для тестов лока (не запускать руками).
// Режимы: hold — держит лок; guard — проверяет отказ при живом владельце
// (exit 3); quit / quit1 — захватил и вышел 0/1; counted — считает onStop
// в stdout (двойной сигнал). Готовность — файл ready после захвата.
import { writeFileSync } from "node:fs"

const libPath = process.env.LIB_PATH
if (libPath === undefined || libPath === "") throw new Error("LIB_PATH не задан")
const { acquireLock } = await import(libPath)

const [mode, stateDir, ready, stopFlag] = process.argv.slice(2)

let stopCount = 0
const onStop = (): void => {
  stopCount++
  writeFileSync(stopFlag, String(stopCount))
  if (mode === "counted") console.log(`onStop=${stopCount}`)
  // drain-паттерн демонов: доезжаем и выходим сами, до grace не доходит
  setTimeout(() => process.exit(0), 100).unref()
}

if (!acquireLock({ stateDir, onStop, graceMs: 2_000 })) process.exit(3)
writeFileSync(ready, String(process.pid))

if (mode === "quit") setTimeout(() => process.exit(0), 50)
if (mode === "quit1") setTimeout(() => process.exit(1), 50)
setInterval(() => {}, 1_000)
