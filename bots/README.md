# uwuwu-bots — раннер ТГ-ботов из доков реестра

Бот описывается **документом** в реестре вики (папка `Projects/uwuwu/Боты`,
контейнер-док «Боты»), раннер сам его запускает и держит — механизм в стиле
крона: новый бот = новый док, без кода (постановка 20261112152504).
Каждый бот — отдельный ребёнок (`bot-child.ts`) на ядре `lib/hub.ts`:
изоляция сбоев, гашение одного не роняет остальных, вечная сессия живёт в
opencode.db переживая рестарты.

## Док-спека бота

Тайтл — `Бот: <slug>` (слаг — ключ жизненного цикла). Тело:

```markdown
## Спека

```yaml
bot:
  token: "123:AAA"                # токен ТГ-бота (приватный контур вики, за auth)
  owner: yurvon_screamo           # username владельца (private-only ядром)
  model: zai-coding-plan/glm-5.3-flash#max   # provider/id[#variant], обязателен
  workspace: /home/yurvon/origa/marketing    # опц.: каталог сессий и дефа
  enabled: true                   # false = бот погашен (сессия в opencode.db живёт)
```

(методичка — остальное тело дока; системный промпт агента)
```

`## Спека` — первый yaml-фенс тела (frontmatter SiYuan вложенные ключи не
хранит — живая проба 13.11). Неизвестные/дублированные ключи — отказ скана
дока с логом (молчаливый пропуск прячет опечатку владельца). Правка
методички — горячая: деф перечитывается сессией на каждом промпте; правка
токена/модели/workspace/enabled — рестарт ребёнка; снос дока — гашение
(вечная сессия в opencode.db остаётся, память переживёт воссоздание по seed).

## Контракт workspace/дефа/seed

Деф резолвится сервером opencode из `location.directory` сессии. Раннер при
спавне читает directory живой (seeded) сессии и материализует деф туда;
расхождение с `bot.workspace` — warning в лог. Новые сессии создаются в
`bot.workspace`. Seed переехавшей сессии: положить `session` (и `offset`) в
`~/.local/state/uwuwu-bots-<slug>/state.json` до старта раннера — ребёнок
подхватит сессию с памятью.

## Раннер

`bots.ts` — супервизор (systemd user, singleton, Node-pinning «bots»): тик
60с — скан реестра (MCP organize-ноды + kernel exportMdContent) → диф
(`bots-lib.decideActions`: spawn/restart/stop/def-update) → управление
детьми (живость — per-бот lock, усыновление сирот после рестарта бесплатно).
Отказ скана (ядро лежит) — заморозка состава: детей не поднимать и не гасить
по stale-данным.

Рантайм-пути: per-бот каталог `~/.local/share/uwuwu-bots/<slug>/`
(hub.json + agent.md + bot-meta.json — материал из дока-спеки), state
`~/.local/state/uwuwu-bots-<slug>/`, лог `~/.local/share/opencode/log/uwuwu-bots.log`.
Env: `UWUWU_BOTS_HOME` (корень рантайма, дефолт homedir), `UWUWU_BOTS_TICK_SEC`,
`UWUWU_BOTS_TG_API`/`UWUWU_BOTS_E2E`/`UWUWU_BOTS_BATCH_MS`/`UWUWU_BOTS_POLL_SEC`
(тестовые крюки — в systemd-юнит не задаются).

## Запуск

```bash
cd ~/uwuwu-agent/bots && bun install
# строка «bots: kisa» в доке Node-pinning (fail-loud без неё) + папка «Боты»
# systemd (юнит — ~/.config/systemd/user/uwuwu-bots.service, EnvironmentFile=
# uwuwu-siyuan.conf — носитель токена перечитывается рестартом юнита):
systemctl --user daemon-reload && systemctl --user enable --now uwuwu-bots
```

Проверка: `journalctl --user -u uwuwu-bots -f` или лог выше; новый бот —
создать док в реестре и ждать тика.

## Проверка (досмерджевый протокол, mock-TG)

Живой прогон детей против mock-TG и реального opencode-сервера — без
реального Telegram и прод-state (риг в скретче, одноразовый; протокол здесь —
канон):

```bash
E2E=<скретч>/bots-e2e && mkdir -p $E2E/home
bun triage/mock-tg.ts --port 8099 --log $E2E/mock-a.jsonl &   # + 8098 для второго бота
# per-бот каталоги: $E2E/home/.local/share/uwuwu-bots/<slug>/{hub.json,agent.md,bot-meta.json}
# сессии с [e2e]-тайтлом (UWUWU_BOTS_E2E=1) — дример их пропускает
UWUWU_BOTS_HOME=$E2E/home UWUWU_BOTS_TG_API=http://127.0.0.1:8099 \
  UWUWU_BOTS_E2E=1 UWUWU_BOTS_BATCH_MS=3000 UWUWU_BOTS_POLL_SEC=1 bun bots/bot-child.ts <slug>
# апдейт владельца снаружи:
curl -s -X POST http://127.0.0.1:8099/admin/push -H 'content-type: application/json' \
  -d '{"update":{"message":{"message_id":11,"chat":{"id":42,"type":"private"},
       "from":{"username":"owner","is_bot":false},"text":"текст"}}}'
# проверка — jsonl-журнал mock'а (доставки) и лог ребёнка
```

Сценарии (прогон 13.11, задача 20261112152504): ×2 ребёнка параллельно —
ответ из вечной сессии; батч двух сообщений склеивается; стирт в активный
ход; kill -9 посреди хода + рестарт — ход доигрывается без дублей (персист
run/очереди); горячая правка agent.md — следующее сообщение по новой
методичке (в живой сессии модель может скептичнуть против истории — готч
«история сильнее обновления», чистая сессия видит свежий деф); def-update
супервизора по правке дока — agent.md переписан без рестарта ребёнка.
Скан супервизора: контейнер «Боты» дискавери, парсинг спеки, кэш
supervisor.json, enabled: false — без спавна.
