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
    (main/orgls-стиль лямбд). Резолв-контур блочных команд ходит и мимо post —
    проба дерева _post_envelope (getBlockInfo, задача 20261107000209) — поэтому
    патчится и она: kernel.envelope если двойник её несёт, иначе дефолт «дерево
    не знает» (code −1) — для неизвестных id это честное поведение живого ядра.
    """
    impl = kernel.post if hasattr(kernel, "post") else kernel
    envelope = getattr(kernel, "envelope", None) or (
        lambda path, body: (-1, None, "Содержимое блока не найдено"))
    original_post, original_envelope = wiki_ops.post, wiki_ops._post_envelope
    wiki_ops.post = impl
    wiki_ops._post_envelope = envelope
    try:
        yield
    finally:
        wiki_ops.post = original_post
        wiki_ops._post_envelope = original_envelope


def run_cmd(cmd, args, kernel) -> tuple[str, str]:
    """Прогон cmd-функции против двойника: перехват обоих потоков → (stdout, stderr)."""
    out, err = io.StringIO(), io.StringIO()
    with kernel_post(kernel), redirect_stdout(out), redirect_stderr(err):
        cmd(args)
    return out.getvalue(), err.getvalue()
