# claudebar-busy.ps1 - UserPromptSubmit hook for the ClaudeBar "work in progress" dots. ASCII-only
# (Windows PowerShell reads .ps1 without BOM as ANSI, so non-ASCII chars break parsing).
#
# Writes %APPDATA%\claudebar\signals\<session>.busy with the project cwd when Claude starts working.
# ClaudeBar polls that folder (~1s) and animates running dots "..." on the matching editor window row.
# The Stop hook (claudebar-bell.ps1) deletes the .busy file when work finishes (dots -> bell).
# Claude Code passes JSON with cwd and session_id on stdin.
#
# Wire it up in ~/.claude/settings.json (hooks.UserPromptSubmit) - see hooks/README.md.

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

$file = Join-Path $dir "$safe.busy"
[System.IO.File]::WriteAllText($file, $cwd, (New-Object System.Text.UTF8Encoding $false))
