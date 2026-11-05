"""main-wrapper обязан гасить только RuntimeError: ERROR + exit 1 без трейсбека.

Контракт (ревью PR #59, Low#1 — до среза покрыт только живым e2e):
  1) RuntimeError из любой команды → stderr «ERROR: …», exit-код 1, без «Traceback»;
  2) прочие исключения (OSError, ValueError, …) НЕ глотаются — пробрасываются
     наружу с трейсбеком (молчаливого проглатывания нет);
  3) успех — тихий: stdout/stderr пусты, никакого SystemExit (естественный 0;
     внезапный exit на успехе валит тест).

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
или напрямую: python3 tests/test_wiki_ops_main.py
"""
import io
import sys
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import tools.wiki_ops as wiki_ops


def dead_kernel(path, body, attempts=6):
    raise RuntimeError("kernel dead")


def broken_kernel(path, body, attempts=6):
    raise ValueError("не kernel-ошибка")


class RuntimeErrorContract(unittest.TestCase):
    """RuntimeError гасится: «ERROR: …» в stderr, exit 1, без трейсбека."""

    def test_prints_error_and_exits_1_without_traceback(self):
        out, err = io.StringIO(), io.StringIO()
        with patch.object(sys, "argv", ["wiki_ops.py", "find", "тайтл"]), \
                patch.object(wiki_ops, "post", dead_kernel), \
                redirect_stdout(out), redirect_stderr(err):
            with self.assertRaises(SystemExit) as cm:
                wiki_ops.main()
        self.assertEqual(cm.exception.code, 1)
        self.assertIn("ERROR: kernel dead", err.getvalue())
        self.assertNotIn("Traceback", err.getvalue())
        self.assertEqual(out.getvalue(), "")


class OtherExceptionsPropagate(unittest.TestCase):
    """Не-RuntimeError из команды не глотается — уходит наружу с трейсбеком."""

    def test_value_error_propagates_out_of_main(self):
        out, err = io.StringIO(), io.StringIO()
        with patch.object(sys, "argv", ["wiki_ops.py", "find", "тайтл"]), \
                patch.object(wiki_ops, "post", broken_kernel), \
                redirect_stdout(out), redirect_stderr(err):
            with self.assertRaises(ValueError):
                wiki_ops.main()


class Success(unittest.TestCase):
    """Успешная команда тиха: пустые stdout/stderr и никакого SystemExit."""

    def test_quiet_success_without_system_exit(self):
        out, err = io.StringIO(), io.StringIO()
        with patch.object(sys, "argv", ["wiki_ops.py", "find", "тайтл"]), \
                patch.object(wiki_ops, "post", lambda path, body, attempts=6: []), \
                redirect_stdout(out), redirect_stderr(err):
            wiki_ops.main()
        self.assertEqual(out.getvalue(), "")
        self.assertEqual(err.getvalue(), "")


if __name__ == "__main__":
    unittest.main()
