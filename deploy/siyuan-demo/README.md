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

## Часовой пояс (канон JST)

Вся временная ось платформы — JST (Asia/Tokyo): хост (ноут в Японии),
демоны (uwuwu-cron/factory/triage — process TZ хоста) и ядро контейнера
(`Environment=TZ=Asia/Tokyo` в юните). Явный TZ обязателен: базовый образ
b3log/siyuan несёт `TZ=Asia/Shanghai` из Dockerfile, а ядро форматирует
date-клетки досок (`formattedContent`) и frontmatter-даты доков в своей
зоне — без переопределения клетка «Запланировано» кронов уезжала на час
против JST-тайтлов ран-карточек (задача 20261110042724). CLI и демоны
работают в локальной зоне хоста (`chrono::Local`/`formatLocal`) — канон
задаётся окружением, не кодом.

Проверка зоны ядра живьём:

```sh
docker exec siyuan-demo date   # ожидаем JST (+0900)
```

Остаточный артефакт смены зоны (самозалечивается): IAL-даты ядра
(`date`/`updated` фронтматтера при экспорте) — зонелесс-числовики, суффикс
зоны синтезируется при экспорте текущей зоной ядра. Доки, чей последний
апдейт был в +8-эру, экспортируются с суффиксом `+09:00` при +8-числовике —
распарсенный момент на час раньше истинного; следующий апдейт дока
переписывает числовик уже в JST и артефакт исчезает. Единственный
потребитель (`bodyLastmodMs` фабрики) деградирует мягко и только в узком
окне доков, не обновлявшихся через смену зоны; правок задним числом не
делается.

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

## Ретеншен журнала journald (sudo)

Логи юнита пишет системный journald; пакет cachyos-settings жёстко ставит
`SystemMaxUse=50M` на весь `/var/log/journal` — user-стрим вытесняет логи
siyuan-demo за часы, форензика смертей процесса ядра оставалась слепой
(задача 20261109083919-v5mlahe; инцидент 08.11: три смерти за вечер, stdout
утрачен). `LogNamespace=` для rootless user-квадлета невозможен
(mount-namespace + системный `journald@.service`), файл-драйвер
(`LogDriver=k8s-file`) дороже и лечит только этот юнит — выбран подъём
лимита дроп-ином в `/etc` (артефакт — `journald-retention.conf` в этой
папке): юнит не меняется, `journalctl --user -u siyuan-demo.service` и
`podman logs` живут как были (форма без `--user` для user-квадлета читает
системный журнал и отдаёт «No entries»), system-журнал оздоравливается тем
же ходом.

Установка (нужна рука с sudo, затем без пароля):

```sh
sudo mkdir -p /etc/systemd/journald.conf.d
sudo cp deploy/siyuan-demo/journald-retention.conf \
  /etc/systemd/journald.conf.d/50-uwuwu-journal-retention.conf
sudo systemctl kill --signal=SIGHUP systemd-journald   # перечитать конфиг (Type=notify-reload)
```

Проверка:

```sh
systemd-analyze cat-config systemd/journald.conf | grep SystemMaxUse   # последняя директива: 1G
sudo journalctl -b -u systemd-journald | grep -i "max"                 # «… is NN, max 1.0G»
```

Первая строка — без sudo: cat-config печатает итоговый merge всех
конфигов/дроп-инов, наша директива последняя (перекрывает пакетную).
Строка с «max 1.0G» — живое подтверждение от работающего journald
(system-стрим без sudo не читается).

Имя `50-…` перекрывает пакетный `00-journal-size.conf` (лексикографически
позднее присваивание выигрывает); пакетный файл не трогаем — обновление
cachyos-settings его перезаписало бы. Откат: удалить
`/etc/systemd/journald.conf.d/50-uwuwu-journal-retention.conf` + SIGHUP —
возврат к пакетным 50M.

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
