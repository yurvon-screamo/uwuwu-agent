// hub-cells-state-child.ts — спавн-ребёнок пина проводки state (не запускать
// руками): импортирует hub из argv-каталога (родитель подставляет и прод-lib,
// и патченную копию для red-check), зовёт read_database_cells против
// фейкового gh на PATH (canned-ответы — env FAKE_GH_LABELS/FAKE_GH_ISSUES)
// и пишет rows в файл-приёмник. Сеть не трогает.
const [libDir, out] = process.argv.slice(2)
if (libDir === undefined || out === undefined) throw new Error("argv: <libDir> <out>")
const { hubTool } = await import(`${libDir}/platform-hub-tools.ts`)
const r = await hubTool<{ rows?: unknown[] }>("read_database_cells", { databaseBlockId: "db:t1073sbx" })
await Bun.write(out, JSON.stringify(r.rows ?? []))
