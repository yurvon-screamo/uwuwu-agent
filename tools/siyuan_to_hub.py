#!/usr/bin/env python3
"""Миграция uwuwu: SiYuan → uwuwu-hub (срез E, задача 20261112155302-ttyp35g).

One-shot: доски всех проектов → issues (тела, комменты, типы, статусы,
frontmatter Branch/PR/Сессия dev/Запланировано/Расписание), вики-дерево
(Experience/Projects/Daily) → docs/, манифест row-id→issue# как артефакт
верификации.

Usage: python3 tools/siyuan_to_hub.py --repo yurvon-screamo/uwuwu-hub \
         [--skip-wiki] [--skip-tasks] [--project NAME] [--dry-run]

Прочее: токен — UWUWU_SIYUAN_TOKEN; gh — текущий аккаунт. Freeze демонов
и бэкап воркспейса — на вызывающем (см. тело задачи, срез E порядок).
"""
import argparse
import json
import os
import re
import subprocess
import sys
import time
import urllib.request

SIYUAN_URL = os.environ.get("UWUWU_SIYUAN_URL", "http://127.0.0.1:6806")
SIYUAN_TOKEN = os.environ.get("UWUWU_SIYUAN_TOKEN", "")

NOTEBOOKS = {"Experience": "Experience", "Projects": "Projects", "Daily": "Daily"}
STATUS_MAP = {
    "Draft": "Draft", "Todo": "Todo", "Design": "Design",
    "Ревью плана": "Ревью плана", "In Progress": "In Progress",
    "Ревью результата": "Ревью результата", "Review": "Review",
    "Done": "Done", "Needs attention": "Needs attention", "Вейт": "Вейт",
}
TASK_TYPES = {"bug", "spike", "analys", "docs", "other", "feature",
              "refactoring", "legacy", "idea", "qa", "infrastructure"}


def siyuan_api(path, payload):
    req = urllib.request.Request(
        SIYUAN_URL + path,
        data=json.dumps(payload).encode(),
        headers={"Authorization": f"Token {SIYUAN_TOKEN}", "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=120) as r:
        d = json.load(r)
    if d.get("code") != 0:
        raise RuntimeError(f"siyuan {path}: {d.get('msg')}")
    return d.get("data")


# ——— MCP-транспорт (render досок): прямых REST endpoint'ов render в ядре
# нет (/api/riff/* не существуют — 404/пусто, проба 13.11); CLI SiYuan-эры
# ходил через POST /mcp (JSON-RPC, tool «database», action «render») —
# паритет cli/src/siyuan.rs mcp_call_once.

MCP_SID = None


def mcp_post(method, params, notify=False):
    global MCP_SID
    body = {"jsonrpc": "2.0", "method": method, "params": params}
    if not notify:
        body["id"] = 1
    hdrs = {"Authorization": f"Token {SIYUAN_TOKEN}",
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream"}
    if MCP_SID:
        hdrs["Mcp-Session-Id"] = MCP_SID
    req = urllib.request.Request(SIYUAN_URL + "/mcp",
                                 data=json.dumps(body).encode(), headers=hdrs)
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            text = r.read().decode()
            sid = r.headers.get("Mcp-Session-Id")
    except urllib.error.HTTPError as e:
        text = e.read().decode()
        if e.code == 404 and MCP_SID:
            # стейл-сессия (рестарт ядра): ре-инициализация и один повтор
            MCP_SID = None
            mcp_initialize()
            return mcp_post(method, params, notify)
        raise RuntimeError(f"mcp {method}: HTTP {e.code}: {text[:200]}")
    if sid:
        MCP_SID = sid
    if not text.strip():
        return None  # notification → 202/пусто
    for line in reversed(text.splitlines()):
        if line.startswith("data: "):
            return json.loads(line[6:])
    return json.loads(text)


def mcp_initialize():
    mcp_post("initialize", {
        "protocolVersion": "2025-03-26", "capabilities": {},
        "clientInfo": {"name": "siyuan-to-hub", "version": "1"}})
    try:
        mcp_post("notifications/initialized", {}, notify=True)
    except Exception:
        pass


def mcp_tool(name, args):
    global MCP_SID
    if not MCP_SID:
        mcp_initialize()
    r = mcp_post("tools/call", {"name": name, "arguments": args})
    if isinstance(r, dict) and "result" in r:
        r = r["result"]
    if r is None:
        raise RuntimeError(f"mcp {name}: пустой ответ")
    if r.get("isError"):
        raise RuntimeError((r.get("content") or [{}])[0].get("text") or f"{name} isError")
    sc = r.get("structuredContent")
    payload = sc if sc is not None else json.loads(
        (r.get("content") or [{}])[0].get("text") or "{}")
    if payload.get("ok") is False:
        raise RuntimeError(payload.get("msg") or payload.get("error") or "ok=false")
    return payload


def gh(args, stdin=None):
    full = list(args)
    if stdin is not None:
        # gh api без --input - игнорирует stdin: тело мутации молча терялось
        full += ["--input", "-"]
    delays = [0, 60, 120, 300, 600]
    r = None
    for attempt, delay in enumerate(delays):
        if delay:
            time.sleep(delay)
        r = subprocess.run(["gh"] + full, input=stdin, capture_output=True, text=True)
        err = r.stderr[:300]
        transient = ("secondary rate limit" in err.lower()
                     or "temporarily blocked" in err.lower()
                     or "something went wrong" in err.lower()
                     or "abuse detection" in err.lower())
        if r.returncode == 0:
            return r.stdout
        if not transient or attempt == len(delays) - 1:
            with open("/home/yurvon/agents-env/20261112155302-ttyp35g/scratch/failed-payload.json", "w") as f:
                f.write(stdin or "")
            raise RuntimeError(f"gh {' '.join(full[:3])}: {err}")
        print(f"  ! transient (попытка {attempt+1}): {err[:120]} — retry {delay}s")
    raise RuntimeError("gh: исчерпаны ретраи")


def gh_json(args, stdin=None):
    return json.loads(gh(args, stdin) or "{}")


GH_RETRY_DELAYS = [2, 5, 15, 30]


def gh_json_retry(args, stdin=None):
    """POST-мутации: secondary rate limit и transient GraphQL сбои —
    ретраи с бэкоффом (2/5/15/30s)."""
    for i, delay in enumerate([0] + GH_RETRY_DELAYS):
        if delay:
            time.sleep(delay)
        try:
            return gh_json(args, stdin)
        except RuntimeError as e:
            text = str(e)
            transient = ("secondary rate limit" in text.lower()
                         or "something went wrong" in text.lower()
                         or "temporarily blocked" in text.lower())
            if i == len(GH_RETRY_DELAYS) or not transient:
                raise
            print(f"  ! gh transient (попытка {i+1}): {text[:120]} — retry через {delay}s")


def sql(stmt):
    return siyuan_api("/api/query/sql", {"stmt": stmt}) or []


# ——— инвентарь ———

def task_boards():
    """Доки-доски: hpath «/<proj>/Задачи <proj>» (2 сегмента)."""
    # SiYuan SQL без явного LIMIT молча отдаёт первые 64 строки — доски
    # за пределами первой порции терялись (проба 13.11). Явный LIMIT 500.
    rows = sql("select id, hpath, box from blocks where type='d' and hpath like '/%/Задачи %' limit 500")
    boards = []
    for r in rows:
        seg = r["hpath"].strip("/").split("/")
        if len(seg) != 2:
            continue
        project = seg[0]
        boards.append({"project": project, "doc_id": r["id"], "box": r["box"]})
    return boards


def board_rows(av_id):
    items = []
    page = 0
    while True:
        d = siyuan_api("/api/riff/renders", {}) if False else None
        break
    return items


MAX_RENDER_PAGES = 100


def resolve_av(doc_id):
    """(avID, blockID носителя) — паритет resolve_av CLI SiYuan-эры."""
    rows = sql(f"select id, markdown from blocks where root_id='{doc_id}' "
               f"and (type='av' or (type='i' and markdown like '%avID=%')) limit 5")
    for r in rows:
        md = r.get("markdown") or ""
        m = re.search(r'avID="([^"]+)"', md) or re.search(r'data-av-id="([^"]+)"', md)
        if m:
            return m.group(1), r["id"]
    return "", ""


def render_board(doc_id):
    """Полная выборка database render через MCP (POST /mcp, tool database,
    action render; серверный pageSize 50). Колонки → keyID-мап для cell_value,
    дедуп по id строки, терминация по rowCount/короткой странице."""
    av_id, block_id = resolve_av(doc_id)
    if not av_id:
        print(f"  ! доска {doc_id}: av-блок не резолвится — skip")
        return []
    rows, columns, seen = [], {}, set()
    page = 1
    while page <= MAX_RENDER_PAGES:
        v = mcp_tool("database", {"action": "render", "id": av_id,
                                  "blockID": block_id, "page": page})
        data = v.get("data") or v
        view = data.get("view") or {}
        for col in view.get("columns") or view.get("fields") or []:
            columns[col["id"]] = col.get("name", "")
        page_rows = view.get("rows") or []
        for r in page_rows:
            rid = r.get("id") or ""
            if rid and rid in seen:
                continue
            seen.add(rid)
            for c in r.get("cells") or []:
                vv = c.get("value") or {}
                k = vv.get("keyID")
                if k in columns:
                    c["name"] = columns[k]
            rows.append(r)
        print(f"  render p{page}: {len(page_rows)} строк")
        if not page_rows:
            break
        row_count = view.get("rowCount")
        if isinstance(row_count, int) and len(rows) >= row_count:
            break
        if len(page_rows) < 50:
            break
        page += 1
    return rows


def cell_value(row, key):
    for cell in row.get("cells") or []:
        if cell.get("name") != key:
            continue
        v = cell.get("value") or {}
        t = v.get("type")
        if t == "date":
            return (v.get("formattedContent") or "", str(v.get("content") or "0"))
        if t in ("mSelect", "select"):
            opts = v.get("mSelect") or v.get("select") or []
            return (", ".join(o.get("content", "") for o in opts), "0")
        if t == "block":
            return ((v.get("block") or {}).get("content") or "", "0")
        if t == "checkbox":
            return ("✓" if v.get("checkbox") else "", "0")
        return (v.get("content") or "", "0")
    return ("", "0")


def row_body_md(doc_id, token_cache):
    if not doc_id:
        return ""
    if doc_id in token_cache:
        return token_cache[doc_id]
    try:
        d = siyuan_api("/api/export/exportMdContent", {"id": doc_id})
        md = (d.get("content") or "")
    except Exception:
        md = ""
    token_cache[doc_id] = md
    return md


def comments_of(md):
    """Секция «## Комменты» тела: маркерные строки → плоский список."""
    lines = md.splitlines()
    out, in_comments, buf = [], False, None
    fenced = False
    for line in lines:
        if line.strip().startswith("```") or line.strip().startswith("~~~"):
            fenced = not fenced
            continue
        if fenced:
            continue
        if line.strip() == "## Комменты":
            in_comments = True
            continue
        if in_comments and line.startswith("## "):
            in_comments = False
        if in_comments and line.strip().startswith("- **"):
            if buf:
                out.append(buf)
            buf = line.strip()
        elif in_comments and buf and line.strip() and not line.startswith("  -"):
            if line.startswith("  -"):
                buf += "\n" + line.strip()
            else:
                out.append(buf)
                buf = None
    if buf:
        out.append(buf)
    return out


def issue_title_of(title):
    return title.replace("/", "∕")


# ——— импорт issues ———

def import_issue(repo, project, title, body_md, status, task_type, branch, pr,
                 dev_session, planned, schedule, priority, autodone,
                 comments, manifest, dry, body_doc_id=""):
    safe_title = issue_title_of(title)
    # frontmatter — единый для обеих веток (status = пайп-статус канона,
    # НЕ GitHub state; Branch/PR/Сессия dev/Запланировано/Расписание —
    # status-дубль для CLI-чтения)
    fm = {"status": status}
    if branch: fm["branch"] = branch
    if pr: fm["pr"] = pr
    if dev_session: fm["dev-session"] = dev_session
    if planned: fm["planned"] = planned
    if schedule: fm["schedule"] = schedule
    if priority: fm["priority"] = priority
    if autodone: fm["autodone"] = autodone
    body_full = "---\n" + "".join(f"{k}: {v}\n" for k, v in fm.items()) \
        + "---\n" + body_md
    existing = gh_json(["api", "-X", "GET", f"repos/{repo}/issues",
                        "-f", f"labels=proj:{project}", "-F", "state=all",
                        "-F", "per_page=100"])
    for issue in existing if isinstance(existing, list) else []:
        if str(issue.get("title", "")).lower() == safe_title.lower():
            num = issue["number"]
            # тело из SiYuan — источник истины миграции: дополняем, если
            # hub-версия короче (волна 1 писала пустые шапки); hub-правки
            # длиннее снапшота не затираем
            if len(body_full) > len(str(issue.get("body") or "")):
                gh(["api", "-X", "PATCH", f"repos/{repo}/issues/{num}"],
                   json.dumps({"body": body_full}))
                print(f"  дубль: «{title}» → #{num} (тело дополнено)")
                tag = "exists+body"
            else:
                print(f"  дубль: «{title}» → #{num} (skip)")
                tag = "exists"
            # канбан + state: Status/Запланировано в Project v2 и
            # Done↔closed синхронизируются и для существующих (ранние
            # прогоны создавали issues без записи канбана — статусы
            # «мигрировали» только в fm тела)
            want_closed = status == "Done"
            is_closed = issue.get("state") == "closed"
            sync_project_fields(repo, project, num, issue.get("node_id", ""), fm)
            if want_closed != is_closed:
                gh(["api", "-X", "PATCH", f"repos/{repo}/issues/{num}"],
                   json.dumps({"state": "closed" if want_closed else "open",
                               **({"state_reason": "completed"} if want_closed else {})}))
            manifest.append({"row_id": manifest_row(title), "issue": num,
                             "title": title, "status": tag})
            return num
    if dry:
        print(f"  [dry] issue «{safe_title}» ({status})")
        return None
    payload = {"title": safe_title, "body": body_full,
               "labels": [f"proj:{project}"]}
    ty = task_type if task_type in TASK_TYPES else None
    if ty:
        payload["type"] = {"bug": "bug", "feature": "feature"}.get(ty, ty)
    created = gh_json_retry(["api", "-X", "POST", f"repos/{repo}/issues"],
                            json.dumps(payload))
    number, node_id = created["number"], created.get("node_id", "")
    sync_project_fields(repo, project, number, node_id, fm)
    if status == "Done":
        gh(["api", "-X", "PATCH", f"repos/{repo}/issues/{number}"],
           json.dumps({"state": "closed", "state_reason": "completed"}))
    for c in comments:
        gh(["api", "-X", "POST", f"repos/{repo}/issues/{number}/comments"],
           json.dumps({"body": c}))
    print(f"  «{title}» → #{number} ({status})")
    return number


def manifest_row(title):
    return title  # row-id → тайтл резолв живой строкой (SiYuan id не в hub)


PROJECT_CACHE = {}


def date_iso(raw):
    """SiYuan date → ISO YYYY-MM-DD (форматы '2026-01-05'/'20260105')."""
    digits = re.sub(r"\D", "", str(raw or ""))[:8]
    if len(digits) == 8:
        return f"{digits[:4]}-{digits[4:6]}-{digits[6:]}"
    return ""


# Нативные поля канбана Project v2 (канон срез A + кастомные под колонки
# SiYuan-доски): статус канбана — поле, не тело. fm в теле остаётся только
# зеркалом для CLI-чтения (контракт GitHubStore).
NATIVE_FIELDS = [
    ("Status", "SINGLE_SELECT",
     ["Draft", "Todo", "Design", "Ревью плана", "In Progress",
      "Ревью результата", "Review", "Done", "Needs attention", "Вейт"]),
    ("Запланировано", "DATE", None),
    ("Приоритет", "SINGLE_SELECT", ["high", "medium", "low"]),
    ("Autodone", "SINGLE_SELECT", ["on", "off"]),
    ("Branch", "TEXT", None),
    ("PR", "TEXT", None),
    ("Сессия dev", "TEXT", None),
    ("Расписание", "TEXT", None),
]

FIELD_COLORS = {"high": "RED", "medium": "YELLOW", "low": "GRAY",
                "on": "GREEN", "off": "GRAY"}


def project_ctx(project):
    """id проекта + все поля (имя → {id, options}) — кэш на прогон."""
    if project in PROJECT_CACHE:
        return PROJECT_CACHE[project]
    d = gh_json(["api", "graphql", "--input", "-"], json.dumps({
        "query": "query { viewer { projectsV2(first: 30) "
                 "{ nodes { id number title } } } }"}))
    pid = None
    for n in d["data"]["viewer"]["projectsV2"]["nodes"]:
        if n["title"] == f"Задачи {project}":
            pid = n["id"]
    if pid is None:
        PROJECT_CACHE[project] = None
        return None
    q = ('query($id: ID!) { node(id: $id) { ... on ProjectV2 { '
         'fields(first: 40) { nodes { '
         '... on ProjectV2Field { id name } '
         '... on ProjectV2SingleSelectField { id name options { id name } } '
         '} } } } }')
    d2 = gh_json(["api", "graphql", "--input", "-"], json.dumps(
        {"query": q, "variables": {"id": pid}}))
    fields = {}
    for f in ((d2["data"]["node"] or {}).get("fields") or {}).get("nodes") or []:
        fields[f["name"]] = {"id": f["id"],
                             "options": {o["name"]: o["id"]
                                         for o in f.get("options") or []}}
    ctx = {"id": pid, "fields": fields}
    PROJECT_CACHE[project] = ctx
    return ctx


def ensure_native_fields(ctx, project):
    """Поля канона, которых нет в проекте, — создаём (Status/Запланировано
    уже залиты срезом A; Приоритет/Autodone/Branch/PR/Сессия dev/Расписание
    добираются первым прогоном мигратора)."""
    fields = ctx["fields"]
    for name, dtype, options in NATIVE_FIELDS:
        if name in fields:
            continue
        inp = {"projectId": ctx["id"], "name": name, "dataType": dtype}
        if options:
            # create строже update: color и description опций обязательны
            # (не null — проба 13.11)
            inp["singleSelectOptions"] = [
                {"name": o, "color": FIELD_COLORS.get(o, "GRAY"),
                 "description": f"канон: {o}"}
                for o in options]
        m = ("mutation($inp: CreateProjectV2FieldInput!) { createProjectV2Field("
             "input: $inp) { projectV2Field { "
             "... on ProjectV2Field { id name } "
             "... on ProjectV2SingleSelectField { id name options { id name } } "
             "} } }")
        try:
            d = gh_json(["api", "graphql", "--input", "-"], json.dumps(
                {"query": m, "variables": {"inp": inp}}))
            f = d["data"]["createProjectV2Field"]["projectV2Field"]
            fields[name] = {"id": f["id"], "options": {
                o["name"]: o["id"] for o in f.get("options") or []}}
            print(f"  поле «{name}» создано в «Задачи {project}»")
        except RuntimeError as e:
            print(f"  ! поле «{name}» ({project}): {str(e)[:120]}")


def project_items_map(ctx):
    """Все items проекта: number → {item_id, fields{name → value}}.
    Пагинация first:100/after (uwuwu-доска > 700 items). Кэш в ctx —
    дифф-запись: пишем только расхождения, повторные прогоны дёшевы."""
    if "items" in ctx:
        return ctx["items"]
    out, after = {}, None
    while True:
        q = ('query($id: ID!, $after: String) { node(id: $id) { '
             '... on ProjectV2 { items(first: 100, after: $after) { '
             'pageInfo { hasNextPage endCursor } nodes { id '
             'content { ... on Issue { number } } '
             'fieldValues(first: 20) { nodes { '
             '... on ProjectV2ItemFieldSingleSelectValue { name '
             'field { ... on ProjectV2Field { name } '
             '... on ProjectV2SingleSelectField { name } } } '
             '... on ProjectV2ItemFieldDateValue { date '
             'field { ... on ProjectV2Field { name } } } '
             '... on ProjectV2ItemFieldTextValue { text '
             'field { ... on ProjectV2Field { name } } } '
             '} } } } } } }')
        d = gh_json(["api", "graphql", "--input", "-"], json.dumps(
            {"query": q, "variables": {"id": ctx["id"], "after": after}}))
        conn = ((d["data"]["node"] or {}).get("items")) or {}
        for it in conn.get("nodes") or []:
            num = ((it.get("content") or {}).get("number"))
            if num is None:
                continue
            fv = {}
            for f in it.get("fieldValues", {}).get("nodes") or []:
                fname = ((f.get("field") or {}).get("name"))
                val = f.get("name") or f.get("date") or f.get("text")
                if fname and val is not None:
                    fv[fname] = str(val)
            out[num] = {"item_id": it["id"], "fields": fv}
        info = conn.get("pageInfo") or {}
        if info.get("hasNextPage"):
            after = info.get("endCursor")
        else:
            break
    ctx["items"] = out
    return out


# GitHub GraphQL не имеет скаляра JSON — каждая форма value своей
# мутацией: select → singleSelectOptionId (String), date → Date!-скаляр,
# text → text (String)
FIELD_MUTS = {
    "select": ("mutation($p: ID!, $i: ID!, $f: ID!, $o: String!) { "
               "updateProjectV2ItemFieldValue(input: {projectId: $p, "
               "itemId: $i, fieldId: $f, value: {singleSelectOptionId: $o}})"
               " { clientMutationId } }", "o"),
    "date": ("mutation($p: ID!, $i: ID!, $f: ID!, $d: Date!) { "
             "updateProjectV2ItemFieldValue(input: {projectId: $p, "
             "itemId: $i, fieldId: $f, value: {date: $d}}) "
             "{ clientMutationId } }", "d"),
    "text": ("mutation($p: ID!, $i: ID!, $f: ID!, $t: String!) { "
             "updateProjectV2ItemFieldValue(input: {projectId: $p, "
             "itemId: $i, fieldId: $f, value: {text: $t}}) "
             "{ clientMutationId } }", "t"),
}


def set_item_field(ctx, item_id, field_id, value, label, kind="select"):
    m, var = FIELD_MUTS[kind]
    for delay in (0, 2, 4, 8):
        if delay:
            time.sleep(delay)  # свежий item виден update'у с лагом (13.11)
        try:
            gh_json(["api", "graphql", "--input", "-"], json.dumps({
                "query": m, "variables": {
                    "p": ctx["id"], "i": item_id, "f": field_id,
                    var: value}}))
            return True
        except RuntimeError as e:
            t = str(e)
            if ("Could not resolve to a node" in t
                    or "was provided invalid value" in t):
                continue
            print(f"  ! {label}: {t[:140]}")
            return False
    print(f"  ! {label}: item не принял запись после ретраев (лаг платформы)")
    return False


def sync_project_fields(repo, project, number, node_id, fm):
    """Все нативные поля канбана из fm: Status/Приоритет/Autodone (select),
    Запланировано (date), Branch/PR/Сессия dev/Расписание (text).
    Item — из карты проекта; нет → addProjectV2ItemById (идемпотентно)."""
    ctx = project_ctx(project)
    if not ctx:
        return
    ensure_native_fields(ctx, project)
    items = project_items_map(ctx)
    cur = items.get(int(number))
    if cur is None:
        m = ("mutation($p: ID!, $c: ID!) { addProjectV2ItemById(input: "
             "{projectId: $p, contentId: $c}) { item { id } } }")
        got = None
        try:
            d = gh_json(["api", "graphql", "--input", "-"], json.dumps({
                "query": m, "variables": {"p": ctx["id"], "c": node_id}}))
            got = ((((d.get("data") or {}).get("addProjectV2ItemById")
                     or {}).get("item") or {}).get("id"))
        except RuntimeError as e:
            print(f"  ! add-item #{number}: {str(e)[:120]}")
        if not got:
            print(f"  ! #{number}: item в Project v2 не создан")
            return
        cur = {"item_id": got, "fields": {}}
        items[int(number)] = cur
    item_id, curf = cur["item_id"], cur["fields"]
    fields = ctx["fields"]

    def want(name, cur_value, write_value, label, kind="select"):
        # сравнение/кэш по значению как его отдаёт items-map (имя опции),
        # запись — по id опции (singleSelectOptionId ждёт id, не имя)
        if fields.get(name) and cur_value is not None \
                and curf.get(name) != str(cur_value):
            if set_item_field(ctx, item_id, fields[name]["id"], write_value,
                              f"#{number} {label}", kind):
                curf[name] = str(cur_value)

    st = fm.get("status") or ""
    sf = fields.get("Status") or {}
    opt = (sf.get("options") or {}).get(st)
    if st and opt:
        want("Status", st, opt, f"Status={st}")
    elif st and not opt:
        print(f"  ! #{number}: опция Status «{st}» не найдена")
    iso = date_iso(fm.get("planned"))
    if iso:
        want("Запланировано", iso, iso, f"Запланировано={iso}", kind="date")
    pr_val = (fm.get("priority") or "").strip().lower()
    pf = (fields.get("Приоритет") or {}).get("options") or {}
    if pr_val in pf:
        want("Приоритет", pr_val, pf[pr_val], f"Приоритет={pr_val}")
    adf = (fields.get("Autodone") or {}).get("options") or {}
    ad = "on" if fm.get("autodone") == "true" else "off"
    if ad in adf:
        want("Autodone", ad, adf[ad], f"Autodone={ad}")
    for fname, key in (("Branch", "branch"), ("PR", "pr"),
                       ("Сессия dev", "dev-session"),
                       ("Расписание", "schedule")):
        val = str(fm.get(key) or "").strip()
        if val:
            want(fname, val, val, f"{fname}={val[:40]}", kind="text")


# ——— вики ———

def export_notebooks(docs_dir):
    """Ноутбуки Experience/Projects/Daily → docs/<NB>/<hpath>.md: плоская
    выгрузка по полному SQL-списку блоков (type='d'), поштучный
    exportMdContent. Полный корпус — никаких walk-усечений."""
    nbs = siyuan_api("/api/notebook/lsNotebooks", {}) or []
    by_name = {n["name"]: n["id"] for n in (nbs.get("notebooks") or [])}
    count, failed = 0, 0
    for nb_name, out_dir in NOTEBOOKS.items():
        nb_id = by_name.get(nb_name)
        if not nb_id:
            print(f"! ноутбук «{nb_name}» не найден")
            continue
        # SiYuan SQL без LIMIT отдаёт дефолт-порцию (64) — пагинируем
        all_docs, offset = [], 0
        while True:
            page = sql(f"select id, hpath from blocks where box='{nb_id}' and type='d' limit 200 offset {offset}")
            all_docs.extend(page)
            if len(page) < 200:
                break
            offset += 200
        print(f"«{nb_name}»: {len(all_docs)} доков")
        for i, r in enumerate(all_docs):
            hpath = r["hpath"].strip("/")
            if not hpath:
                # корневой док ноутбука: <NB>.md
                dest = os.path.join(docs_dir, out_dir, f"{nb_name}.md")
            else:
                segs = [s2.replace("/", "∕") for s2 in hpath.split("/")]
                dest = os.path.join(docs_dir, out_dir, *segs) + ".md"
            os.makedirs(os.path.dirname(dest), exist_ok=True)
            try:
                d = siyuan_api("/api/export/exportMdContent", {"id": r["id"]})
                with open(dest, "w") as f:
                    f.write(d.get("content") or "")
                count += 1
            except Exception as e:
                failed += 1
                if failed <= 5:
                    print(f"  ! «{hpath}»: {e}")
            if (i + 1) % 100 == 0:
                print(f"  «{nb_name}»: {i + 1}/{len(all_docs)}")
    if failed:
        print(f"! не выгружено: {failed}")
    return count


# ——— main ———

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", default="yurvon-screamo/uwuwu-hub")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--project", default=None, help="один проект (доска)")
    ap.add_argument("--skip-wiki", action="store_true")
    ap.add_argument("--skip-tasks", action="store_true")
    ap.add_argument("--fill-priority", action="store_true",
                    help="Дозаполнить priority/autodone у уже мигрированных issues (волна 1 до этих колонок)")
    ap.add_argument("--clone-root", default=os.path.expanduser(
        "~/.local/share/uwuwu-cli/hub-clone"))
    args = ap.parse_args()

    if args.fill_priority:
        # Дозаполнение волны 1: issue из hub по тайтлу → priority/autodone
        # из SiYuan строки → PATCH body (fm). Волна 1 шла до этих колонок.
        boards = task_boards()
        if args.project:
            boards = [b for b in boards if b["project"] == args.project]
        patched = 0
        for b in boards:
            print(f"fill «{b['project']}»:")
            rows = render_board(b["doc_id"])
            board_hpath = None
            bd = sql(f"select hpath from blocks where id='{b['doc_id']}' limit 1")
            if bd:
                board_hpath = bd[0].get("hpath")
            for row in rows:
                title = ""
                for cell in row.get("cells") or []:
                    v = cell.get("value") or {}
                    if v.get("type") == "block":
                        title = (v.get("block") or {}).get("content") or ""
                        break
                if not title:
                    continue
                # doc_id тела задачи: док-саб по hpath "<доска>/<тайтл>"
                body_doc_id = ""
                if board_hpath:
                    safe_title = title.replace("'", "''")
                    bd2 = sql(f"select id from blocks where type='d' and hpath='{board_hpath}/{safe_title}' limit 1")
                    if bd2:
                        body_doc_id = bd2[0].get("id", "")
                pr = cell_value(row, "Приоритет")[0]
                ad_raw = cell_value(row, "Autodone")[0].strip()
                ad = "true" if ad_raw not in ("", "false") else "false"
                issues = gh_json(["api", "-X", "GET", f"repos/{args.repo}/issues",
                                  "-F", "state=all", "-F", "per_page=100"], None)
                target = next((i for i in (issues if isinstance(issues, list) else [])
                               if str(i.get("title", "")).lower() == title.lower()), None)
                if not target:
                    continue
                body = str(target.get("body") or "")
                fm_needed = {}
                if pr and f"priority: {pr}" not in body:
                    fm_needed["priority"] = pr
                if ad_raw and f"autodone: {ad}" not in body:
                    fm_needed["autodone"] = ad
                if not fm_needed:
                    continue
                fm = {"priority": pr, "autodone": ad}
                if args.dry_run:
                    print(f"  [dry] #{target['number']} <- {sorted(fm_needed)}")
                    continue
                fm_text = "---" + chr(10) + "".join(
                    f"{k}: {v}" + chr(10) for k, v in fm.items()) + "---" + chr(10) + body
                gh(["api", "-X", "PATCH", f"repos/{args.repo}/issues/{target['number']}"],
                   json.dumps({"body": fm_text}))
                patched += 1
                print(f"  #{target['number']} <- {sorted(fm_needed)}")
        print(f"fill: {patched} issues дополнены")
        return

    if not args.skip_tasks:
        boards = task_boards()
        if args.project:
            boards = [b for b in boards if b["project"] == args.project]
        print(f"доски: {[b['project'] for b in boards]}")
        manifest = []
        cache = {}
        for b in boards:
            print(f"проект «{b['project']}»:")
            rows = render_board(b["doc_id"])
            board_hpath = None
            bd = sql(f"select hpath from blocks where id='{b['doc_id']}' limit 1")
            if bd:
                board_hpath = bd[0].get("hpath")
            for row in rows:
                # Primary Key канон разъехался по доскам (Primary Key /
                # Название / Первичный ключ) — берём первую block-клетку
                title = ""
                for cell in row.get("cells") or []:
                    v = cell.get("value") or {}
                    if v.get("type") == "block":
                        title = (v.get("block") or {}).get("content") or ""
                        break
                if not title:
                    title = (cell_value(row, "Первичный ключ")[0]
                             or cell_value(row, "Primary Key")[0]
                             or cell_value(row, "Название")[0]).strip()
                if not title:
                    continue
                status = STATUS_MAP.get(cell_value(row, "Status")[0], cell_value(row, "Status")[0])
                ty = cell_value(row, "Тип")[0]
                priority = cell_value(row, "Приоритет")[0]
                autodone_raw = cell_value(row, "Autodone")[0]
                autodone = "true" if autodone_raw.strip() not in ("", "false") else "false"
                branch = cell_value(row, "Branch")[0]
                pr = cell_value(row, "PR")[0]
                session = cell_value(row, "Сессия dev")[0]
                planned, planned_ms = cell_value(row, "Запланировано")
                schedule = cell_value(row, "Расписание")[0]
                # doc_id тела задачи: док-саб по hpath "<доска>/<тайтл>"
                body_doc_id = ""
                if board_hpath:
                    safe_title = title.replace("'", "''")
                    bd2 = sql(f"select id from blocks where type='d' and hpath='{board_hpath}/{safe_title}' limit 1")
                    if bd2:
                        body_doc_id = bd2[0].get("id", "")
                body = row_body_md(body_doc_id, cache)
                comments = comments_of(body)
                number = import_issue(args.repo, b["project"], title, body, status,
                                      ty, branch, pr, session, planned, schedule,
                                      priority, autodone,
                                      comments, manifest, args.dry_run, body_doc_id)
                time.sleep(2)
                manifest.append({"row_id": body_doc_id, "issue": number, "title": title,
                                 "status": status, "project": b["project"]})
        with open("migration-manifest.json", "w") as f:
            json.dump(manifest, f, ensure_ascii=False, indent=1)
        print(f"манифест: {len(manifest)} строк → migration-manifest.json")

    if not args.skip_wiki:
        docs_dir = os.path.join(args.clone_root,
                                args.repo.replace("/", "__"), "docs")
        n = export_notebooks(docs_dir)
        print(f"вики: {n} доков → {docs_dir}")
        if not args.dry_run:
            for msg in ("wiki: миграция SiYuan → hub (пачка)",
                        "wiki: миграция SiYuan → hub"):
                r = subprocess.run(["git", "-C", os.path.dirname(docs_dir), "add", "-A"],
                                   capture_output=True, text=True)
                if r.returncode == 0:
                    break
            subprocess.run(["git", "-C", os.path.dirname(docs_dir),
                            "-c", "user.name=AGENTS", "-c", "user.email=agents@uwuwu.net",
                            "commit", "-m", "wiki: миграция SiYuan → hub (одна пачка)"],
                           capture_output=True, text=True)
            subprocess.run(["git", "-C", os.path.dirname(docs_dir), "push", "origin", "main"],
                           capture_output=True, text=True)
            print("wiki: push origin/main")


if __name__ == "__main__":
    main()
