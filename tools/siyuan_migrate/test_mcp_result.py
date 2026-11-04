#!/usr/bin/env python3
"""Контракт mcp_result: не-dict result от шлюза → McpError, а не тихий {}.

tools/call по JSON-RPC обязан возвращать result-объект; строка/список/null —
мусор шлюза. Прежняя ветка молча возвращала {} — мусорный ответ на мутации
стрелял ниже по цепочке с невнятной ошибкой (последнее нарушение доктрины
McpError, PR #11 / ADR-010: любой не-договорённый ответ = исключение).

На старом коде тест красный, на новом — зелёный. Без pytest-инфры
(в репо только vitest-TS): plain-asserts + check()-хелпер + раннер с явным
списком TESTS — незарегистрированный тест молча не исполняется
(experience /testing «Тест-раннер с явным списком кейсов»).

Запуск: python3 tools/siyuan_migrate/test_mcp_result.py  (exit 0 = все зелёные)
"""

from __future__ import annotations

import sys
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS))

import triage_affine as ta  # noqa: E402

RESULTS: list[tuple[str, bool, str]] = []


def check(name: str, fn) -> None:
    try:
        fn()
        RESULTS.append((name, True, ""))
        print(f"ok   {name}")
    except Exception as e:
        RESULTS.append((name, False, repr(e)))
        print(f"FAIL {name}: {e!r}")


def t_non_dict_result_raises_mcperror() -> None:
    """result строкой/списком/null → McpError с подстрокой «не-dict result».

    Три формы — один концепт: любое не-dict значение result нарушает
    контракт JSON-RPC; falsy-кейс {} остаётся валидным и не проверяется.
    """
    garbage_forms = ["строка-мусор", [1, 2], None]
    for garbage in garbage_forms:
        # act + assert
        try:
            ta.mcp_result({"result": garbage})
        except ta.McpError as e:
            assert "не-dict result" in str(e), f"{garbage!r}: сообщение {str(e)!r} без подстроки"  # nosec B101 — assert: контракт тест-раннера
        else:
            raise AssertionError(f"не-dict result {garbage!r} прошёл молча")


TESTS = [
    ("не-dict result → McpError (строка/список/null)", t_non_dict_result_raises_mcperror),
]


def main() -> int:
    for name, fn in TESTS:
        check(name, fn)
    failed = [name for name, ok, _ in RESULTS if not ok]
    print(f"\nитого: {len(RESULTS) - len(failed)}/{len(RESULTS)} ok")
    if failed:
        print("провалены: " + "; ".join(failed))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
