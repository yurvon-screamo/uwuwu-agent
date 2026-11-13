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


def render_board(doc_id):
    """Полная выборка database render через uwuwu-cli kernel render
    (--page пагинация, pageSize 50; колонки → keyID-мап для cell_value)."""
    rows, columns, page = [], {}, 1
    while True:
        out = subprocess.run(["uwuwu-cli", "kernel", "render", doc_id,
                              "--page", str(page)],
                             capture_output=True, text=True)
        if out.returncode != 0:
            raise RuntimeError(f"kernel render {doc_id} p{page}: {out.stderr[:300]}")
        d = json.loads(out.stdout)
        data = d.get("data") or {}
        view = data.get("view") or {}
        for col in view.get("columns") or []:
            columns[col["id"]] = col["name"]
        page_rows = view.get("rows") or []
        print(f"  render p{page}: {len(page_rows)} строк")
        if not page_rows:
            break
        for r in page_rows:
            # keyID → имя колонки (cell_value ищет name)
            for cell in r.get("cells") or []:
                v = cell.get("value") or {}
                key = v.get("keyID")
                if key in columns:
                    cell["name"] = columns[key]
        rows.extend(page_rows)
        if len(page_rows) < data.get("pageSize", 50):
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
    existing = gh_json(["api", "-X", "GET", f"repos/{repo}/issues",
                        "-f", f"labels=proj:{project}", "-F", "state=all",
                        "-F", "per_page=100"])
    for issue in existing if isinstance(existing, list) else []:
        if str(issue.get("title", "")).lower() == safe_title.lower():
            # тело из SiYuan полнее hub-версии (волна 1 писала пустые тела) —
            # дополняем: PATCH body если hub короче SiYuan
            hub_body = str(issue.get("body") or "")
            if len(body_md) > len(hub_body):
                fm_now = {"status": str(issue.get("state", "open")).lower()}
                gh(["api", "-X", "PATCH", f"repos/{repo}/issues/{issue['number']}"],
                   json.dumps({"body": "---" + chr(10) + "".join(
                       f"{k}: {v}" + chr(10) for k, v in fm_now.items()) + "---" + chr(10) + body_md}))
                print(f"  дубль: «{title}» → #{issue['number']} (тело дополнено)")
                manifest.append({"row_id": manifest_row(title), "issue": issue["number"],
                                 "title": title, "status": "exists+body"})
                return issue["number"]
            print(f"  дубль: «{title}» → #{issue['number']} (skip)")
            manifest.append({"row_id": manifest_row(title), "issue": issue["number"],
                             "title": title, "status": "exists"})
            return issue["number"]
    if dry:
        print(f"  [dry] issue «{safe_title}» ({status})")
        return None
    payload = {"title": safe_title, "body": body_md,
               "labels": [f"proj:{project}"]}
    ty = task_type if task_type in TASK_TYPES else None
    if ty:
        payload["type"] = {"bug": "bug", "feature": "feature"}.get(ty, ty)
    number = gh_json_retry(["api", "-X", "POST", f"repos/{repo}/issues"],
                           json.dumps(payload))["number"]
    # frontmatter-ячейки поверх тела (Branch/PR/Сессия dev/Запланировано/
    # Расписание + status-дубль для CLI-чтения)
    fm = {"status": status}
    if branch: fm["branch"] = branch
    if pr: fm["pr"] = pr
    if dev_session: fm["dev-session"] = dev_session
    if planned: fm["planned"] = planned
    if schedule: fm["schedule"] = schedule
    if priority: fm["priority"] = priority
    if autodone: fm["autodone"] = autodone
    if fm:
        body = "---\n" + "".join(f"{k}: {v}\n" for k, v in fm.items()) + "---\n" + body_md
        gh(["api", "-X", "PATCH", f"repos/{repo}/issues/{number}"],
           json.dumps({"body": body}))
    if status and status != "Draft":
        set_project_status(project, number, status)
        time.sleep(1.5)
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


def set_project_status(project, number, status):
    try:
        proj_number = None
        d = gh_json(["api", "graphql", "--input", "-"], json.dumps({
            "query": "query { viewer { projectsV2(first: 30) { nodes { number title } } } }"}))
        needle = f"Задачи {project}"
        for n in d["data"]["viewer"]["projectsV2"]["nodes"]:
            if n["title"] == needle:
                proj_number = n["number"]
        if proj_number is None:
            return
        q = ("query($n: Int!) { viewer { projectV2(number: $n) { id "
             "items(first: 100) { nodes { id content { ... on Issue { number } } } } "
             "statusField: field(name: \"Status\") { ... on ProjectV2SingleSelectField "
             "{ id options { id name } } } } } }")
        d2 = gh_json(["api", "graphql", "--input", "-"], json.dumps({"query": q, "variables": {"n": proj_number}}))
        proj = d2["data"]["viewer"]["projectV2"]
        item = next((i for i in proj["items"]["nodes"]
                     if i["content"].get("number") == int(number)), None)
        opt = next((o for o in proj["statusField"]["options"] if o["name"] == status), None)
        if not item or not opt:
            return
        m = ("mutation($p: ID!, $i: ID!, $f: ID!, $o: String!) { "
             "updateProjectV2ItemFieldValue(input: {projectId: $p, itemId: $i, "
             "fieldId: $f, value: {singleSelectOptionId: $o}}) { clientMutationId } }")
        gh(["api", "graphql", "--input", "-"], json.dumps({
            "query": m, "variables": {"p": proj["id"], "i": item["id"],
                                       "f": proj["statusField"]["id"], "o": opt["id"]}}))
    except Exception as e:
        print(f"  ! Projects v2 status «{status}»: {e}")


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
