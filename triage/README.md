# uwuwu-triage — входная точка задач через Telegram

Бот-триажер: юзер кидает описание задачи в личку боту → демон поднимает
opencode-сессию триаж-агента (`glm-5.3-flash`, изолированный workspace) →
агент классифицирует (проект/тип), дедуплицирует и заводит задачу в uwuwu
(`task create`) или дополняет существующую (`task comment`) → финал прогона
доставляется юзеру в ТГ сводкой. Референс-архитектура — origa marketing zoo
(standalone bun-демон, не плагин; опыт `opencode-tg-hub-daemon`).

## Файлы

- `triage.ts` — демон: TG long-poll → спавн сессии → поллинг финала → ответ
- `triage-lib.ts` — чистые функции (тесты без сети: `bun test`)
- `prompt.md` — методичка триаж-агента (в git; правки подхватываются без кода)
- `hub.json` — секреты (gitignored): `{ "token": "<TG-бот>", "owner": "<username>"}`
- `hub.example.json` — шаблон конфига

## Рантайм-пути (вне репо)

- state (offset, чат, очередь, активный прогон): `~/.local/state/uwuwu-triage/state.json`
- лог: `~/.local/share/opencode/log/uwuwu-triage.log`
- workspace сессий: `~/.local/share/uwuwu-triage/workspace` (пустой, изоляция)

Env для `uwuwu-cli` внутри сессий наследуется от opencode-сервера, не от демона.

## Запуск

```bash
cd ~/uwuwu-agent/triage && bun install
cp hub.example.json hub.json  # заполнить token/owner
# systemd (юнит — ~/.config/systemd/user/uwuwu-triage.service):
systemctl --user daemon-reload && systemctl --user enable --now uwuwu-triage
```

Проверка: `journalctl --user -u uwuwu-triage -f` или лог-файл выше; юзер
пишет боту `/start`, получает справку.

## Поведение

- Принимаются только личные сообщения владельца (`owner` из hub.json); чужие — тихо.
- Один прогон одновременно; занят → заявка в очередь (персистится, рестарт не теряет).
- Финал = последнее assistant-сообщение сессии (REST, DESC); markdown → TG HTML, plain-фолбэк.
- Прогон > 15 мин → юзеру предупреждение, наблюдение отпускается.
- Рестарт демона во время прогона → дозаявка не дублируется: прогон дозирается по `state.run`.
