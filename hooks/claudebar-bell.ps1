# claudebar-bell.ps1 - Stop/Notification hook for the ClaudeBar bell. ASCII-only on purpose
# (Windows PowerShell reads .ps1 without BOM as ANSI, so non-ASCII chars break parsing).
#
# Writes %APPDATA%\claudebar\signals\<session>.signal with the project cwd. ClaudeBar polls
# that folder (~1s) and highlights the editor window row whose project name matches.
# Claude Code passes JSON with cwd and session_id on stdin.
#
# Wire it up in ~/.claude/settings.json (hooks.Stop) - see hooks/README.md or -=letter=-.txt.

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
[System.IO.File]::WriteAllText($file, $cwd, (New-Object System.Text.UTF8Encoding $false))
