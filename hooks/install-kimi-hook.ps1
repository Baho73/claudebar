# install-kimi-hook.ps1 - idempotently adds ClaudeBar terminal-status hooks to Kimi Code CLI.
# Kimi Code CLI reads ~/.kimi-code/config.toml (TOML [[hooks]] array). Its hook payload carries the
# SAME fields as Claude Code (cwd, session_id on stdin), so it reuses the SAME scripts:
#   UserPromptSubmit + PostToolUse -> claudebar-busy.ps1  (writes <session>.busy, keep-alive)
#   Stop                            -> claudebar-bell.ps1  (writes <session>.signal, removes .busy)
# Each session writes its own <session_id>.busy, so ClaudeBar counts one status square per session.
# On Windows, Kimi runs hook commands through Git Bash; powershell.exe must be on PATH.
# Run: powershell -ExecutionPolicy Bypass -File "D:\Python\claudebar\hooks\install-kimi-hook.ps1"
# Makes a config.toml.bak backup; re-running does not duplicate. ASCII-only on purpose.

$ErrorActionPreference = 'Stop'

$dir = Join-Path $env:USERPROFILE '.kimi-code'
$f = Join-Path $dir 'config.toml'
$busy = 'D:\Python\claudebar\hooks\claudebar-busy.ps1'
$bell = 'D:\Python\claudebar\hooks\claudebar-bell.ps1'
$alive = 'D:\Python\claudebar\hooks\claudebar-alive.ps1'
$end = 'D:\Python\claudebar\hooks\claudebar-end.ps1'
$startMarker = '# --- ClaudeBar terminal status hooks'

New-Item -ItemType Directory -Force -Path $dir | Out-Null

$existing = ''
if (Test-Path $f) {
    Copy-Item $f "$f.bak" -Force
    $existing = Get-Content $f -Raw
    if ($null -eq $existing) { $existing = '' }
}
# Idempotent + upgrade-safe: strip any previously managed block (from start marker to EOF), then re-append full set.
$idx = $existing.IndexOf($startMarker)
if ($idx -ge 0) { $existing = $existing.Substring(0, $idx).TrimEnd() }

# TOML literal strings (single quotes) keep backslashes as-is; command keeps the quoted .ps1 path.
$block = @"

# --- ClaudeBar terminal status hooks (managed by install-kimi-hook.ps1 - do not edit inside) ---
[[hooks]]
event = "SessionStart"
command = 'powershell -NoProfile -ExecutionPolicy Bypass -File "$alive" -Agent kimi'
timeout = 10

[[hooks]]
event = "UserPromptSubmit"
command = 'powershell -NoProfile -ExecutionPolicy Bypass -File "$busy"'
timeout = 10

[[hooks]]
event = "PostToolUse"
command = 'powershell -NoProfile -ExecutionPolicy Bypass -File "$busy"'
timeout = 10

[[hooks]]
event = "Stop"
command = 'powershell -NoProfile -ExecutionPolicy Bypass -File "$bell"'
timeout = 10

[[hooks]]
event = "SessionEnd"
command = 'powershell -NoProfile -ExecutionPolicy Bypass -File "$end"'
timeout = 10
"@

[System.IO.File]::WriteAllText($f, $existing + $block, (New-Object System.Text.UTF8Encoding $false))
Write-Output "OK: ClaudeBar hooks (presence + busy + bell) written to $f (backup: $f.bak if it existed)"
