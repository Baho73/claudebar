# claudebar-bell.ps1 — Stop/Notification-хук Claude Code для «звоночка» ClaudeBar.
#
# Пишет файл-сигнал %APPDATA%\claudebar\signals\<session>.signal с путём проекта (cwd).
# Панель ClaudeBar опрашивает эту папку (~1с) и подсвечивает строку окна, чьё имя
# проекта = имя папки cwd. Подсветка гаснет, когда окно проекта получает фокус.
#
# Claude Code передаёт хуку на stdin JSON с полями cwd и session_id.
#
# Подключение в ~/.claude/settings.json (массив hooks.Stop, рядом с notify-flash):
#   {
#     "type": "command",
#     "command": "powershell -NoProfile -ExecutionPolicy Bypass -File \"D:\\Python\\claudebar\\hooks\\claudebar-bell.ps1\""
#   }
#
# Сброс «зависших» сигналов делает сама панель (по фокусу окна); файл-сигнал —
# одна штука на сессию (session_id), перезаписывается при каждом срабатывании.

$ErrorActionPreference = 'SilentlyContinue'

$raw = [Console]::In.ReadToEnd()
if ([string]::IsNullOrWhiteSpace($raw)) { return }

try { $j = $raw | ConvertFrom-Json } catch { return }

$cwd = $j.cwd
if ([string]::IsNullOrWhiteSpace($cwd)) { $cwd = (Get-Location).Path }
if ([string]::IsNullOrWhiteSpace($cwd)) { return }

$sid = $j.session_id
if ([string]::IsNullOrWhiteSpace($sid)) { $sid = 'default' }
$safe = ($sid -replace '[^\w\-]', '_')

$dir = Join-Path $env:APPDATA 'claudebar\signals'
New-Item -ItemType Directory -Force -Path $dir | Out-Null

$file = Join-Path $dir "$safe.signal"
Set-Content -LiteralPath $file -Value $cwd -Encoding UTF8
