# claudebar-alive.ps1 - SessionStart hook: presence marker for ClaudeBar status squares.
# Writes %APPDATA%\claudebar\signals\<session>.alive with the project cwd and the agent name.
# ClaudeBar draws one square per live session on the project row, colored by agent
# (Claude teal / Kimi violet). State (idle/working/done) comes from .busy/.signal of the same session.
# SessionEnd (claudebar-end.ps1) removes the marker. Pass -Agent claude|kimi from the hook command.
# ASCII-only on purpose (Windows PowerShell reads .ps1 without BOM as ANSI).
param([string]$Agent = 'claude')

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

$file = Join-Path $dir "$safe.alive"
[System.IO.File]::WriteAllText($file, "cwd=$cwd`nagent=$Agent", (New-Object System.Text.UTF8Encoding $false))
