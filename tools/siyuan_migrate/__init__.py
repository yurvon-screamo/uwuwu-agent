"""uwuwu → SiYuan migration tooling.

Stages (each resumable):
  export        AFFiNE organize tree + doc markdown + blob manifest → local dir
  transform     html_embed fences → raw HTML, blob refs normalization
  import        create notebooks/docs in SiYuan via Kernel API, upload assets
  boards        kanban boards «Задачи <Proj>» → SiYuan database + kanban view

Usage: python3 -m siyuan_migrate.export [--out DIR] [--limit N] [--notebook X]
"""
