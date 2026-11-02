# uwuwu-dreamer: автозапуск на Windows через Task Scheduler.
#
# Регистрирует задачу "\UWUWU\Dreamer": старт при логоне юзера, рестарт при
# падении (раз в минуту, до 10 попыток), без лимита времени выполнения
# (демон живёт вечно). Идемпотентно: -Force перезаписывает задачу.
#
# Дубли не страшны: dreamer.ts держит single-instance lock — второй инстанс
# тихо выходит (exit 1), задача это штатно переживает.
#
# Использование:
#   powershell -NoProfile -ExecutionPolicy Bypass -File win-autostart.ps1          # зарегистрировать
#   powershell -NoProfile -ExecutionPolicy Bypass -File win-autostart.ps1 -RunNow  # + запустить сейчас
#
# Почему Register-ScheduledTask, а не schtasks /xml: на redmi schtasks /create
# с логон-триггером отдаёт "Access is denied" (обычные задачи при этом
# создаются); Register-ScheduledTask работает без админских прав.
#
# Поведение LogonType=Interactive ("run only when user is logged on"):
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

$action = New-ScheduledTaskAction -Execute $bun -Argument "dreamer.ts" -WorkingDirectory $scriptDir
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
$settings = New-ScheduledTaskSettingsSet `
  -RestartCount 10 `
  -RestartInterval (New-TimeSpan -Minutes 1) `
  -ExecutionTimeLimit ([TimeSpan]::Zero) `
  -MultipleInstances IgnoreNew `
  -AllowStartIfOnBatteries `
  -DontStopIfGoingOnBatteries `
  -StartWhenAvailable
$principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive

Register-ScheduledTask -TaskPath $TaskPath -TaskName $TaskName `
  -Action $action -Trigger $trigger -Settings $settings -Principal $principal `
  -Description "uwuwu-dreamer: демон пост-обработки сессий opencode (dreamer.ts). Установка: dreamer/win-autostart.ps1." `
  -Force | Out-Null

Write-Host "задача '$TaskPath$TaskName' зарегистрирована:"
Write-Host "  bun:    $bun"
Write-Host "  скрипт: $dreamerTs"

if ($RunNow) {
  Start-ScheduledTask -TaskPath $TaskPath -TaskName $TaskName
  Write-Host "запущена вручную (при живом демоне второй инстанс выйдет по lock — это штатно)"
}
