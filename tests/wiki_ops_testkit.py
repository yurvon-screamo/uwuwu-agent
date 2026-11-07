"""Общий скаффолдинг прогонов wiki_ops против фейкового ядра.

Ручная подмена wiki_ops.post (save/patch/restore) повторялась в run-хелперах
каждого тест-файла — здесь она одна (low ревью PR #109, задача
20261107055005). FakeKernel-двойники остаются в своих файлах намеренно:
каждый эмулирует свою API-поверхность ядра (create/delete/blocks/… — разные
контракты), слияние дало бы конфигуратор вместо двойника.

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
"""
import io
from contextlib import contextmanager, redirect_stderr, redirect_stdout

import tools.wiki_ops as wiki_ops


@contextmanager
def kernel_post(kernel):
    """wiki_ops.post → post двойника на время блока, восстановление в finally.

    Принимает двойника ядра (объект с .post) или сырую post-функцию
    (main/orgls-стиль лямбд)."""
    impl = kernel.post if hasattr(kernel, "post") else kernel
    original = wiki_ops.post
    wiki_ops.post = impl
    try:
        yield
    finally:
        wiki_ops.post = original


def run_cmd(cmd, args, kernel) -> tuple[str, str]:
    """Прогон cmd-функции против двойника: перехват обоих потоков → (stdout, stderr)."""
    out, err = io.StringIO(), io.StringIO()
    with kernel_post(kernel), redirect_stdout(out), redirect_stderr(err):
        cmd(args)
    return out.getvalue(), err.getvalue()
