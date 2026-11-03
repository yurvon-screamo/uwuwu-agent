#!/usr/bin/env bash
# Полный конвейер миграции: ждёт завершения экспорта, затем transform → blobs → import → boards.
set -uo pipefail
cd "$(dirname "$0")/.."   # tools/
LOG=/tmp/opencode/spike-affine/pipeline.log
exec >>"$LOG" 2>&1
echo "=== pipeline started $(date -u +%FT%TZ) ==="

# 1. дождаться экспортёра (запущен отдельным nohup-процессом)
while pgrep -f "python3 -m siyuan_migrate.export" >/dev/null; do sleep 20; done
echo "=== export finished $(date -u +%FT%TZ) ==="
python3 - <<'EOF'
import json
m = json.load(open('/home/yurvon/siyuan-migration-work/export/manifest.json'))
ok = sum(1 for i in m['docs'].values() if i.get('ok'))
print(f"export result: ok={ok} failed={len(m['errors'])} blobs={len(m['blobs'])}")
EOF

# 2. transform
python3 -m siyuan_migrate.transform || echo "TRANSFORM FAIL"

# 3. blobs (env уже в окружении)
python3 -m siyuan_migrate.blobs || echo "BLOBS FAIL"

# 4. assets upload + map
python3 -m siyuan_migrate.import_siyuan --assets || echo "ASSETS FAIL"

# 5. docs import (идемпотентно)
python3 -m siyuan_migrate.import_siyuan || echo "IMPORT FAIL"

# 6. boards + comments
python3 -m siyuan_migrate.boards --comments || echo "BOARDS FAIL"

echo "=== pipeline done $(date -u +%FT%TZ) ==="
