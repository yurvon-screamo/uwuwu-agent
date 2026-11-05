# siyuan-watch — вотчдер контейнера siyuan-demo

Минутный systemd user-таймер, который держит прод-контур SiYuan поднятым:
TCP-проба `127.0.0.1:6806` (транспорт-детект) → если порт мёртв, инцидент-строка
в `~/.local/state/siyuan-watch/watch.log` и `podman start siyuan-demo`
(на running-контейнере — no-op, безопасно). Hang-классы (контейнер Up, порт
мёртв — pasta-клин / вис туннеля) вотчдер только детектит и логирует; их
хил-лестница — осознанно вне среза (задача 20261105224609).

Контекст класса «тихие остановки» и рецепт атрибуции SIGTERM —
док SiYuan: devops/«SiYuan-контур uwuwu», режим 8.

## Зачем

- rootless podman без `podman-system-service` не обслуживает container-level
  рестарт-политику: `unless-stopped` инертен, при смерти/ребуте контейнер не
  поднимет никто (факт верифицирован 05.11, см. режим 8).
- Watchdog — минимальное закрытие дыры без миграции контейнера на quadlet
  (quadlet — отдельно, по желанию владельца).

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
journalctl --user -u siyuan-watch.service -n 20     # тишина = всё здорово
```

Живое даун-окно (осознанный прод-эксперимент, ~минута 502 на публичном UI):

```sh
docker stop siyuan-demo    # docker на хосте — shim над podman
sleep 70
docker ps --filter name=siyuan-demo   # контейнер снова Up
cat ~/.local/state/siyuan-watch/watch.log   # инцидент-строка с state-фактами
```

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
