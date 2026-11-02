﻿# uwuwu-dreamer: автозапуск на Windows через Task Scheduler.
#
# Регистрирует задачу "\UWUWU\Dreamer": вечный тик каждую минуту (TimeTrigger
# с Repetition, старт от момента установки) + старт при логоне юзера.
# Живой инстанс отсекается IgnoreNew, мёртвый — поднимается следующим тиком:
# падение демона живёт не дольше ~1 мин.
#
# Почему так, а не RestartOnFailure / Repetition на LogonTrigger:
# - "restart on failure" на Win11 24H2 (unified scheduling engine) срабатывает
#   только на невозможность СТАРТА задачи, на смерть процесса — нет
#   (проверено живьём: kill bun-процесса → рестарта нет);
# - Repetition на LogonTrigger тикает от события логона — в уже открытой
#   сессии не срабатывает (проверено живьём);
# - вечный TimeTrigger переживает ребут (тики идут по стенному времени),
#   LogonTrigger даёт немедленный старт после логона, StartWhenAvailable
#   добирает пропущенные на время сна/выключения запуски.
#
# XML строится вручную: New-ScheduledTaskTrigger сериализует Repetition как
# StopAtDurationEnd=true без Duration — такая задача не тикает.
# schtasks /create здесь тоже мимо: с логон-триггером отдаёт Access denied
# (обычные задачи при этом создаются), поэтому Register-ScheduledTask -Xml.
#
# Дубли не страшны и на других слоях: IgnoreNew в самой задаче, а single-instance
# lock в dreamer.ts отсекает запуски ВНЕ задачи (ручные, параллельные) — второй
# инстанс тихо выходит (exit 1).
#
# Использование:
#   powershell -NoProfile -ExecutionPolicy Bypass -File win-autostart.ps1          # зарегистрировать
#   powershell -NoProfile -ExecutionPolicy Bypass -File win-autostart.ps1 -RunNow  # + запустить сейчас
#
# Поведение LogonType=InteractiveToken ("run only when user is logged on"):
# дочерние сессии дримера наследуют пользовательское окружение
# (HKCU\Environment), включая UWUWU_* креды. Планировщик с S4U /
# "run whether user is logged on" их не подхватывает — не переключать.

param([switch]$RunNow)

$ErrorActionPreference = "Stop"
$TaskPath = "\UWUWU\"
$TaskName = "Dreamer"

# — реальный bun.exe (npm-обёртка bun — shim, планировщику нужен exe) —
$bunCandidates = @(
  (Join-Path $env:USERPROFILE ".bun\bin\bun.exe"),
  (Join-Path $env:APPDATA "npm\node_modules\bun\bin\bun.exe")
)
$bun = $bunCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $bun) {
  $bun = (Get-Command bun.exe -ErrorAction SilentlyContinue).Source
}
if (-not $bun) { throw "bun.exe не найден — установи bun или добавь путь в bunCandidates" }

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$dreamerTs = Join-Path $scriptDir "dreamer.ts"
if (-not (Test-Path $dreamerTs)) { throw "не найден $dreamerTs" }

# — юзер с доменным квалификатором (на domain/Entra-joined машинах bare-имя
#   может не совпасть с логоном — задача молча не стартанёт) —
$taskUser = "$env:USERDOMAIN\$env:USERNAME"

# — XML задачи; StartBoundary = момент установки, Repetition без Duration =
#   вечное повторение (StopAtDurationEnd=false) —
$startBoundary = (Get-Date).ToString("yyyy-MM-ddTHH:mm:ss")
$xml = @"
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>uwuwu-dreamer: демон пост-обработки сессий opencode (dreamer.ts). Установка: dreamer/win-autostart.ps1.</Description>
  </RegistrationInfo>
  <Triggers>
    <TimeTrigger>
      <Repetition>
        <Interval>PT1M</Interval>
        <StopAtDurationEnd>false</StopAtDurationEnd>
      </Repetition>
      <StartBoundary>$startBoundary</StartBoundary>
      <Enabled>true</Enabled>
    </TimeTrigger>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>$taskUser</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>$taskUser</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>$bun</Command>
      <Arguments>dreamer.ts</Arguments>
      <WorkingDirectory>$scriptDir</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"@

Register-ScheduledTask -TaskPath $TaskPath -TaskName $TaskName -Xml $xml -Force | Out-Null

Write-Host "задача '$TaskPath$TaskName' зарегистрирована:"
Write-Host "  bun:    $bun"
Write-Host "  скрипт: $dreamerTs"
Write-Host "  юзер:   $taskUser (тик каждую минуту + старт при логоне)"

if ($RunNow) {
  Start-ScheduledTask -TaskPath $TaskPath -TaskName $TaskName
  Write-Host "запущена вручную (при живом демоне старт отсечётся IgnoreNew — это штатно)"
}
