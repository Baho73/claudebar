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
$marker = 'claudebar\hooks\claudebar-'

New-Item -ItemType Directory -Force -Path $dir | Out-Null

$existing = ''
if (Test-Path $f) {
    Copy-Item $f "$f.bak" -Force
    $existing = Get-Content $f -Raw
    if ($null -eq $existing) { $existing = '' }
}
if ($existing -match [regex]::Escape($marker)) {
    Write-Output "already present: $f"
    exit 0
}

# TOML literal strings (single quotes) keep backslashes as-is; command keeps the quoted .ps1 path.
$block = @"

# --- ClaudeBar terminal status hooks (added by install-kimi-hook.ps1) ---
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
"@

[System.IO.File]::WriteAllText($f, $existing + $block, (New-Object System.Text.UTF8Encoding $false))
Write-Output "OK: added ClaudeBar hooks to $f (backup: $f.bak if it existed)"
