"""siyuan_watch — контракт watchdog-итерации контейнера siyuan-demo.

Контракт (задача 20261105224609, план-срез 1; BDD в теле задачи):
  1) проба OK → тихий exit 0: старта нет, строк инцидента нет;
  2) проба fail → state-факты контейнера + одна JSON-строка инцидента в лог
     {ts, event, container_status, exit_code, restarts} + `podman start
     siyuan-demo` (на running — no-op) + верификационная перепроба;
  3) старт не удался (и перепроба не спасла) → exit 1, инцидент уже в логе;
  4) контейнера нет вовсе → state "missing" (факты None), та же логика.

Внешние зависимости (сокет, podman, часы) — на фейках; лог-файл — реальный
tmp-каталог. Прогон: python3 -m unittest discover -s tests -t .  (из корня
репо) или напрямую: python3 tests/test_siyuan_watch.py
"""
import datetime
import json
import socket
import subprocess  # nosec B404 — фейковые CompletedProcess для контракта запуска podman
import tempfile
import unittest
from pathlib import Path

import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import tools.siyuan_watch as siyuan_watch


def completed(stdout="", stderr="", returncode=0):
    return subprocess.CompletedProcess([], returncode, stdout, stderr)


class ProbeTcpTests(unittest.TestCase):
    def test_open_tcp_port_is_healthy(self):
        with socket.create_server(("127.0.0.1", 0)) as listener:
            port = listener.getsockname()[1]
            self.assertTrue(siyuan_watch.probe_tcp(port=port, timeout_s=2))

    def test_refused_tcp_port_is_down(self):
        with socket.create_server(("127.0.0.1", 0)) as listener:
            dead_port = listener.getsockname()[1]
        self.assertFalse(siyuan_watch.probe_tcp(port=dead_port, timeout_s=2))


class ReadContainerStateTests(unittest.TestCase):
    def test_parses_status_exit_code_and_restarts(self):
        payload = json.dumps(
            {
                "State": {"Status": "exited", "ExitCode": 0},
                "RestartCount": 2,
            }
        )
        calls = []

        def runner(cmd, **_):
            calls.append(cmd)
            return completed(stdout=payload)

        state = siyuan_watch.read_container_state(runner=runner)
        self.assertEqual(
            state,
            {"container_status": "exited", "exit_code": 0, "restarts": 2},
        )
        self.assertEqual(calls[0][:2], ["podman", "inspect"])

    def test_absent_container_reports_missing_state(self):
        def runner(cmd, **_):
            return completed(returncode=1, stderr="no such container")

        state = siyuan_watch.read_container_state(runner=runner)
        self.assertEqual(
            state,
            {"container_status": "missing", "exit_code": None, "restarts": None},
        )

    def test_unparseable_inspect_output_reports_missing_state(self):
        def runner(cmd, **_):
            return completed(stdout="<not json>")

        state = siyuan_watch.read_container_state(runner=runner)
        self.assertEqual(state["container_status"], "missing")
        self.assertIsNone(state["exit_code"])


class StartContainerTests(unittest.TestCase):
    def test_successful_start_is_reported_as_success(self):
        def runner(cmd, **_):
            return completed(stdout="siyuan-demo\n")

        ok, _ = siyuan_watch.start_container(runner=runner)
        self.assertTrue(ok)

    def test_already_running_container_counts_as_success(self):
        def runner(cmd, **_):
            return completed(returncode=1, stderr="container is already running")

        ok, _ = siyuan_watch.start_container(runner=runner)
        self.assertTrue(ok)

    def test_failed_start_reports_failure_with_detail(self):
        def runner(cmd, **_):
            return completed(returncode=125, stderr="podman exploded")

        ok, detail = siyuan_watch.start_container(runner=runner)
        self.assertFalse(ok)
        self.assertIn("podman exploded", detail)


class IncidentRecordTests(unittest.TestCase):
    def test_record_carries_fixed_contract_fields(self):
        state = {"container_status": "exited", "exit_code": 0, "restarts": 2}
        record = siyuan_watch.incident_record(state)
        self.assertEqual(
            set(record),
            {"ts", "event", "container_status", "exit_code", "restarts"},
        )
        self.assertEqual(record["event"], "container_down")
        self.assertEqual(record["container_status"], "exited")
        self.assertEqual(record["exit_code"], 0)
        self.assertEqual(record["restarts"], 2)

    def test_ts_is_absolute_time_with_utc_offset(self):
        record = siyuan_watch.incident_record({"container_status": "exited"})
        parsed = datetime.datetime.fromisoformat(record["ts"])
        self.assertIsNotNone(parsed.utcoffset())


class WriteIncidentTests(unittest.TestCase):
    def test_creates_missing_log_directory(self):
        with tempfile.TemporaryDirectory() as tmp:
            log_path = Path(tmp) / "nested" / "dir" / "watch.log"
            siyuan_watch.write_incident(log_path, {"event": "container_down"})
            self.assertTrue(log_path.exists())

    def test_appends_one_json_line_per_incident(self):
        with tempfile.TemporaryDirectory() as tmp:
            log_path = Path(tmp) / "watch.log"
            siyuan_watch.write_incident(log_path, {"event": "first"})
            siyuan_watch.write_incident(log_path, {"event": "second"})
            lines = log_path.read_text(encoding="utf-8").splitlines()
            self.assertEqual([json.loads(line)["event"] for line in lines], ["first", "second"])


class RunOnceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.log_path = Path(self.tmp.name) / "watch.log"

    def test_healthy_probe_is_silent_no_action(self):
        start_calls = []

        exit_code = siyuan_watch.run_once(
            probe=lambda: True,
            read_state=lambda: {},
            start=lambda: start_calls.append("start") or (True, ""),
            log_path=self.log_path,
        )

        self.assertEqual(exit_code, 0)
        self.assertEqual(start_calls, [])
        self.assertFalse(self.log_path.exists())

    def test_down_probe_logs_incident_starts_container_and_verifies(self):
        probes = iter([False, True])
        commands = []

        def start():
            commands.append("start")
            return (True, "")

        exit_code = siyuan_watch.run_once(
            probe=lambda: next(probes),
            read_state=lambda: {"container_status": "exited", "exit_code": 0, "restarts": 1},
            start=start,
            log_path=self.log_path,
        )

        self.assertEqual(exit_code, 0)
        self.assertEqual(commands, ["start"])
        lines = self.log_path.read_text(encoding="utf-8").splitlines()
        self.assertEqual(len(lines), 1)
        record = json.loads(lines[0])
        self.assertEqual(record["event"], "container_down")
        self.assertEqual(record["container_status"], "exited")
        self.assertEqual(record["exit_code"], 0)
        self.assertEqual(record["restarts"], 1)

    def test_failed_start_exits_nonzero_with_incident_logged(self):
        exit_code = siyuan_watch.run_once(
            probe=lambda: False,
            read_state=lambda: {"container_status": "exited", "exit_code": 0, "restarts": 0},
            start=lambda: (False, "boom"),
            log_path=self.log_path,
        )

        self.assertEqual(exit_code, 1)
        self.assertTrue(self.log_path.exists())

    def test_unhealed_container_after_start_exits_nonzero(self):
        exit_code = siyuan_watch.run_once(
            probe=lambda: False,
            read_state=lambda: {"container_status": "exited", "exit_code": 0, "restarts": 0},
            start=lambda: (True, ""),
            log_path=self.log_path,
        )

        self.assertEqual(exit_code, 1)

    def test_running_container_with_dead_port_is_logged_as_hang_signature(self):
        probes = iter([False, False])

        exit_code = siyuan_watch.run_once(
            probe=lambda: next(probes),
            read_state=lambda: {"container_status": "running", "exit_code": 0, "restarts": 0},
            start=lambda: (True, ""),
            log_path=self.log_path,
        )

        self.assertEqual(exit_code, 1)
        record = json.loads(self.log_path.read_text(encoding="utf-8").splitlines()[0])
        self.assertEqual(record["container_status"], "running")


if __name__ == "__main__":
    unittest.main()
