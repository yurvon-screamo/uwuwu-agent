# siyuan-demo — quadlet-юнит контейнера SiYuan (хранилище uwuwu)

Прод-контур SiYuan как systemd user-юнит: quadlet генерит
`siyuan-demo.service` из `~/.config/containers/systemd/siyuan-demo.container`.
Миграция с plain-run — задача 20261107061921 (07.11.2026), закрыла два
отверстия задачи 20261105224609:

- рестарт-политика `--restart unless-stopped` у plain-run контейнера была
  инертна (podman-system-service не запущен) — теперь рестарт при любом exit
  обслуживает systemd (`Restart=always`, `StartLimitIntervalSec=0`);
- conmon/pasta жили в cgroup стартовавшей сессии — теперь собственный скоуп
  юнита (`app.slice/siyuan-demo.service`), привязки к интерактивному
  терминалу нет;
- автостарт на буте: `[Install] WantedBy=default.target` + linger=yes.

Watchdog siyuan-watch остаётся вторым слоем: его хил поднялся с
`podman start` на `systemctl --user start siyuan-demo.service` — контейнер
под quadlet пересоздаётся юнитом при каждом старте (`--replace --rm`), и
внешний `podman start` сломал бы связку. Hang-классы вотчер только
детектит, как и раньше.

## Установка (файлы в этой папке)

```sh
mkdir -p ~/.config/containers/systemd
cp deploy/siyuan-demo/siyuan-demo.container ~/.config/containers/systemd/
# вписать реальный accessAuthCode вместо <accessAuthCode>
# (значение — в conf воркспейса ~/siyuan-demo-ws/conf/conf.json и у владельца)
systemctl --user daemon-reload
systemctl --user start siyuan-demo.service
```

`enable` не нужен и невозможен (юнит generated): wants-линк в
`default.target` создаёт генератор при каждом daemon-reload/буте.

## Проверка

```sh
systemctl --user is-enabled siyuan-demo.service        # generated
loginctl show-user yurvon | grep Linger               # Linger=yes
systemctl --user status siyuan-demo.service           # cgroup: app.slice/siyuan-demo.service
systemctl --user show siyuan-demo.service -p NRestarts
uwuwu-cli kernel ping                                 # alive: sql ok
```

Проверка рестарта при любом exit: `podman kill siyuan-demo` (SIGKILL) →
юнит поднимает контейнер за RestartSec=5, NRestarts растёт
(plain-run: RestartCount оставался 0 при любой смерти).

## Процедура миграции с plain-run (выполнена 07.11, даун-окно ~22с)

1. `systemctl --user stop siyuan-watch.timer` — вотчер не должен дёргать
   `podman start` в окне.
2. Бэкап воркспейса по ранбуку «Апгрейд образа siyuan-demo» (шаг 2:
   `docker stop` → `rsync -a --delete`, ожидаем code 23 от root-runtime
   файлов, сверка числа файлов data/history/conf 1:1).
3. `docker rm siyuan-demo` → пауза ~10с (pasta-гонка порта).
4. Файл `.container` в `~/.config/containers/systemd/` →
   `daemon-reload` → `start` (сухая генерация для валидации — прогнать
   `/usr/lib/systemd/user-generators/podman-user-generator <tmpdir>`).
5. Пробы: kernel ping/sql, публичная цепочка
   (`https://siyuan-relay-production.up.railway.app` → 401), version,
   cgroup юнита, journal без panic/corrupt.
6. `podman kill siyuan-demo` → автоподъём, NRestarts=1.
7. `systemctl --user start siyuan-watch.timer` (после деплоя обновлённого
   хила siyuan-watch).

Откат на plain-run: `systemctl --user stop siyuan-demo.service` →
`docker run` с прежними аргументами (ранбук, шаг 3). Воркспейс общий,
данных откат не требует.
