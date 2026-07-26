# claudebar-end.ps1 - SessionEnd hook: remove this session's ClaudeBar markers (presence/busy/signal).
# Wired to SessionEnd; clears <session>.alive/.busy/.signal so the square disappears when the agent exits.
# ASCII-only on purpose (Windows PowerShell reads .ps1 without BOM as ANSI).

$ErrorActionPreference = 'SilentlyContinue'

$raw = [Console]::In.ReadToEnd()
$sid = $null
if (-not [string]::IsNullOrWhiteSpace($raw)) {
    try { $sid = ($raw | ConvertFrom-Json).session_id } catch { $sid = $null }
}
if ([string]::IsNullOrWhiteSpace($sid)) { $sid = 'default' }
$safe = ($sid -replace '[^\w\-]', '_')

$dir = Join-Path $env:APPDATA 'claudebar\signals'
foreach ($ext in '.alive', '.busy', '.signal') {
    Remove-Item -LiteralPath (Join-Path $dir "$safe$ext") -Force -ErrorAction SilentlyContinue
}
