# install-bell-hook.ps1 - idempotently adds the ClaudeBar bell hook to ~/.claude/settings.json.
# Run: powershell -ExecutionPolicy Bypass -File "D:\Python\claudebar\hooks\install-bell-hook.ps1"
# Makes a settings.json.bak backup; re-running does not duplicate. ASCII-only on purpose.

$ErrorActionPreference = 'Stop'

$f = Join-Path $env:USERPROFILE '.claude\settings.json'
if (-not (Test-Path $f)) { Write-Output "ERR: settings.json not found: $f"; exit 1 }

Copy-Item $f "$f.bak" -Force
$j = Get-Content $f -Raw | ConvertFrom-Json

$cmd = 'powershell -NoProfile -ExecutionPolicy Bypass -File "D:\Python\claudebar\hooks\claudebar-bell.ps1"'

if (-not $j.hooks -or -not $j.hooks.Stop -or @($j.hooks.Stop).Count -eq 0) {
    Write-Output "ERR: hooks.Stop missing - add the block manually (see -=letter=-.txt)"
    exit 1
}

$existing = @($j.hooks.Stop[0].hooks).command
if ($existing -contains $cmd) {
    Write-Output "already present"
    exit 0
}

$j.hooks.Stop[0].hooks = @($j.hooks.Stop[0].hooks) + @([pscustomobject]@{ type = 'command'; command = $cmd })
$json = $j | ConvertTo-Json -Depth 100
[System.IO.File]::WriteAllText($f, $json, (New-Object System.Text.UTF8Encoding $false))
Write-Output "OK: bell hook added"
