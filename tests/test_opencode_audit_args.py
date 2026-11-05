"""opencode_audit.py: обе формы вызова — глобальные флаги до и после сабкоманды.

Контекст (задача 20261104072029): промпт дримера генерил
`session <id> --compact --db <путь>`, argparse падал с `unrecognized arguments`
(глобальные опции объявлены до subparsers, после сабкоманды он их не видит).
Парсер пересобран: parent-парсер с default=SUPPRESS подключён и к главному,
и к каждому сабпарсеру — обе формы эквивалентны, дефолты дозаполняются
после parse_args (parse_intermixed_args с сабпарсерами невозможен —
argparse кидает TypeError на nargs=PARSER).

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
или напрямую: python3 tests/test_opencode_audit_args.py
"""
import io
import sys
import unittest
from contextlib import redirect_stdout
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from tools import opencode_audit as audit


class GlobalArgsOrderTest(unittest.TestCase):
    def test_flags_before_subcommand(self):
        args = audit.parse_cli_args(["--db", "fixtures/x.db", "--compact", "session", "ses_1"])
        self.assertEqual(args.db, Path("fixtures/x.db"))
        self.assertTrue(args.compact)
        self.assertEqual(args.cmd, "session")
        self.assertEqual(args.session_id, "ses_1")

    def test_flags_after_subcommand(self):
        args = audit.parse_cli_args(["session", "ses_1", "--compact", "--db", "fixtures/x.db"])
        self.assertEqual(args.db, Path("fixtures/x.db"))
        self.assertTrue(args.compact)
        self.assertEqual(args.cmd, "session")
        self.assertEqual(args.session_id, "ses_1")

    def test_defaults_intact_when_flags_absent(self):
        args = audit.parse_cli_args(["session", "ses_1"])
        self.assertEqual(args.db, audit.DEFAULT_DB)
        self.assertEqual(args.wiki, audit.DEFAULT_WIKI)
        self.assertFalse(args.compact)
        self.assertEqual(args.limit_user_msgs, audit.LIMIT_USER_MSGS)
        self.assertEqual(args.reasoning_truncate, audit.REASONING_TRUNCATE)

    def test_all_global_flags_accepted_after_subcommand(self):
        argv = [
            "session", "ses_1",
            "--wiki", "fixtures/w",
            "--limit-user-msgs", "5", "--limit-asst-texts", "6",
            "--limit-shell-cmds", "7", "--limit-kid-titles", "8",
            "--text-truncate", "9", "--limit-timeline", "10",
            "--limit-tool-use-samples", "11", "--shell-output-preview", "12",
            "--limit-reasoning", "13", "--reasoning-truncate", "14",
        ]
        args = audit.parse_cli_args(argv)
        self.assertEqual(args.wiki, Path("fixtures/w"))
        self.assertEqual(args.limit_user_msgs, 5)
        self.assertEqual(args.limit_asst_texts, 6)
        self.assertEqual(args.limit_shell_cmds, 7)
        self.assertEqual(args.limit_kid_titles, 8)
        self.assertEqual(args.text_truncate, 9)
        self.assertEqual(args.limit_timeline, 10)
        self.assertEqual(args.limit_tool_use_samples, 11)
        self.assertEqual(args.shell_output_preview, 12)
        self.assertEqual(args.limit_reasoning, 13)
        self.assertEqual(args.reasoning_truncate, 14)

    def test_subcommand_flags_still_work(self):
        args = audit.parse_cli_args(["--compact", "overview", "--days", "3"])
        self.assertEqual(args.cmd, "overview")
        self.assertEqual(args.days, 3)
        self.assertTrue(args.compact)
        self.assertEqual(args.db, audit.DEFAULT_DB)

    def test_help_lists_globals(self):
        # main- и sub-help живы, глобальные опции видны в обоих
        self.assertIn("--db", audit.build_parser().format_help())
        buf = io.StringIO()
        with self.assertRaises(SystemExit), redirect_stdout(buf):
            audit.parse_cli_args(["session", "--help"])
        self.assertIn("--db", buf.getvalue())

    def test_unknown_flag_still_rejected(self):
        with self.assertRaises(SystemExit):
            with redirect_stdout(io.StringIO()):
                audit.parse_cli_args(["session", "ses_1", "--bogus-flag"])


if __name__ == "__main__":
    unittest.main()
