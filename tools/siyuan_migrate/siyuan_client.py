"""Minimal SiYuan Kernel API client (HTTP, token auth).

Covers what the migration needs: notebooks, doc tree creation from markdown,
asset upload, SQL search. Docs: https://github.com/siyuan-note/siyuan/blob/master/docs/API.md
"""
from __future__ import annotations

import json
import time
from pathlib import Path
from urllib.request import Request, urlopen


class SiYuan:
    def __init__(self, base_url: str = "http://127.0.0.1:6806", token: str = ""):
        self.base = base_url.rstrip("/")
        self.token = token

    @classmethod
    def from_workspace_conf(cls, ws_dir: Path, base_url: str = "http://127.0.0.1:6806") -> "SiYuan":
        conf = json.loads((ws_dir / "conf" / "conf.json").read_text())
        return cls(base_url, conf["api"]["token"])

    def _post(self, path: str, payload: dict | list, raw: bytes | None = None,
              content_type: str = "application/json", attempts: int = 6) -> dict:
        if raw is not None:
            body = raw
        else:
            body = json.dumps(payload, ensure_ascii=False).encode()
        last = ""
        for i in range(attempts):
            req = Request(self.base + path, data=body, method="POST")
            req.add_header("Authorization", f"Token {self.token}")
            req.add_header("Content-Type", content_type)
            try:
                with urlopen(req, timeout=60) as resp:
                    data = json.loads(resp.read())
                if data.get("code") == 0:
                    return data.get("data", {})
                last = f"code={data.get('code')} msg={data.get('msg')}"
                if "metadata is not ready" in str(data.get("msg")):
                    time.sleep(2.0 + i)  # ядро индексирует свежий ноутбук — дать время
                    continue
            except Exception as e:  # noqa: BLE001
                last = str(e)
            time.sleep(1.5 * (i + 1))
        raise RuntimeError(f"POST {path} failed: {last}")

    # --- notebooks ---
    def ls_notebooks(self) -> list[dict]:
        return self._post("/api/notebook/lsNotebooks", {}).get("notebooks", [])

    def create_notebook(self, name: str) -> str:
        r = self._post("/api/notebook/createNotebook", {"name": name})
        return r.get("notebook", {}).get("id", "")

    def ensure_notebook(self, name: str) -> str:
        for nb in self.ls_notebooks():
            if not nb.get("closed") and nb.get("name") == name:
                return nb["id"]
        return self.create_notebook(name)

    # --- docs ---
    def create_doc_with_md(self, notebook: str, path: str, markdown: str) -> dict:
        """path like '/folder/subdoc'; returns doc info. Idempotent-ish: on
        conflict SiYuan appends a suffix — migration avoids conflicts upstream."""
        return self._post("/api/filetree/createDocWithMd", {
            "notebook": notebook, "path": path, "markdown": markdown,
        })

    def get_doc_id_by_path(self, notebook: str, path: str) -> str | None:
        try:
            r = self._post("/api/filetree/getIDsByHPath", {"notebook": notebook, "path": path}, attempts=2)
            ids = r if isinstance(r, list) else r.get("ids", [])
            return ids[0] if ids else None
        except Exception:  # noqa: BLE001
            return None

    def rename_doc(self, notebook: str, from_path: str, to_path: str) -> dict:
        return self._post("/api/filetree/renameDocByID", {
            "notebook": notebook, "fromURL": from_path, "toURL": to_path,
        })

    def doc_exists(self, notebook: str, path: str) -> bool:
        return self.get_doc_id_by_path(notebook, path) is not None

    # --- assets ---
    def upload_asset(self, file_path: Path) -> str:
        """Upload file into workspace assets/; returns server path like assets/xxx.png."""
        boundary = "----uwuwumigrate" + str(int(time.time() * 1000))
        fname = file_path.name
        b = file_path.read_bytes()
        body = b"".join([
            f"--{boundary}\r\nContent-Disposition: form-data; name=\"files[]\"; "
            f"filename=\"{fname}\"\r\nContent-Type: application/octet-stream\r\n\r\n".encode(),
            b, f"\r\n--{boundary}--\r\n".encode(),
        ])
        r = self._post("/api/asset/upload", None, raw=body,
                       content_type=f"multipart/form-data; boundary={boundary}")
        succ = r.get("succMap", {})
        # succMap: {originalFilename: serverPath}
        return succ.get(fname, next(iter(succ.values()), ""))

    # --- search/sql ---
    def sql(self, stmt: str) -> list[dict]:
        return self._post("/api/query/sql", {"stmt": stmt}).get("data") or []

    def version(self) -> str:
        return str(self._post("/api/system/version", {}, attempts=2))
