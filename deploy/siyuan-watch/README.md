# siyuan-watch — вотчер контейнера siyuan-demo

Минутный systemd user-таймер — второй слой хила прод-контура SiYuan:
TCP-проба `127.0.0.1:6806` (транспорт-детект) → если порт мёртв, инцидент-строка
в `~/.local/state/siyuan-watch/watch.log` и `systemctl --user start
siyuan-demo.service` (юнит active — no-op, безопасно). Hang-классы (контейнер Up,
порт мёртв — pasta-клин / вис туннеля) вотчер только детектит и логирует; их
хил-лестница — осознанно вне среза (задача 20261105224609).

Контекст класса «тихие остановки» и рецепт атрибуции SIGTERM —
док SiYuan: devops/«SiYuan-контур uwuwu», режим 8.

## Зачем

- С 07.11.2026 контейнер живёт под quadlet-юнитом `siyuan-demo.service`
  (deploy/siyuan-demo/): рестарт при любом exit и автостарт на буте —
  первичный хил systemd. Вотчер дублит хил осознанно (внешний детект
  transport-класса с минутной гранулярностью) и остаётся единственным
  детектором hang-классов, которые systemd не видит.
- До миграции вотчер был единственным механизмом автоподъёма: rootless
  podman без `podman-system-service` не обслуживал container-level
  рестарт-политику (факт верифицирован 05.11, режим 8).
- Хил только через systemd: контейнер под quadlet пересоздаётся юнитом
  (`podman run --replace --rm` на каждый старт), внешний `podman start`
  поднял бы его мимо юнита и сломал связку.

## Установка

Скрипт копируется в стабильное место (`~/.local/bin`) — путь юнита не зависит
от чекаутов и ворктри:

```sh
cp tools/siyuan_watch.py ~/.local/bin/siyuan-watch
cp deploy/siyuan-watch/siyuan-watch.service ~/.config/systemd/user/
cp deploy/siyuan-watch/siyuan-watch.timer   ~/.config/systemd/user/
systemctl --user daemon-reload
loginctl enable-linger yurvon   # юниты переживают logout; на этой машине уже включён
systemctl --user enable --now siyuan-watch.timer
```

Обновление логики — повторить первый шаг (копирование), остальное не трогается.

## Проверка установки

```sh
systemctl --user list-timers siyuan-watch.timer     # тик каждую минуту
systemctl --user status siyuan-watch.service        # без «Failed» = здорово
journalctl --user -u siyuan-watch.service -n 20
```

Пары Starting/Finished в journal — норма каждого тика. О здоровье говорит
отсутствие stderr-строк хила (`failed to start …`, `failed to heal …`) и
статуса Failed у сервиса, а не тишина в journal.

Живое даун-окно (осознанный прод-эксперимент, ~минута 502 на публичном UI):

```sh
systemctl --user stop siyuan-demo.service   # юнит-стоп: Restart=always на stop не срабатывает
sleep 70
systemctl --user is-active siyuan-demo.service   # active — вотчер поднял
cat ~/.local/state/siyuan-watch/watch.log   # инцидент-строка с state-фактами
```

`podman kill siyuan-demo` как сценарий даун-окна больше не годится: смерть
контейнера юнит ловит сам за RestartSec=5 — вотчер не успевает; это и есть
работа первичного хила.

## Контракт лога

`~/.local/state/siyuan-watch/watch.log` — JSON-строки с фиксированными полями:
`ts` (абсолютная метка с TZ-офсетом), `event` (`container_down`),
`container_status` (`exited`/`running`/`missing`/…), `exit_code`,
`restarts`. OK-тики в лог не пишутся. Строка с `container_status=running` при
мёртвом порте — сигнатура hang-класса.

## Границы

- Вотчер смотрит только транспорт с самого хоста; публичная цепочка
  (Railway relay → rathole) им не покрывается — её зонды описаны в опыте
  devops/«SiYuan-контур uwuwu» (проба №2).
- Лечение hang-классов рестартом — не делается (только детект в логе).
