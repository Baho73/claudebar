# install-bell-hook.ps1 - idempotently adds the ClaudeBar hooks to ~/.claude/settings.json:
#   - bell on Stop  (claudebar-bell.ps1): highlights the row when Claude finishes
#   - busy on UserPromptSubmit (claudebar-busy.ps1): running dots when Claude starts
#   - keep-alive on PostToolUse (claudebar-busy.ps1): refreshes .busy mtime while tools run,
#     so the dots persist through a long task and clear ~90s after work stops (staleness)
# Run: powershell -ExecutionPolicy Bypass -File "D:\Python\claudebar\hooks\install-bell-hook.ps1"
# Makes a settings.json.bak backup; re-running does not duplicate. ASCII-only on purpose.

$ErrorActionPreference = 'Stop'

$f = Join-Path $env:USERPROFILE '.claude\settings.json'
if (-not (Test-Path $f)) { Write-Output "ERR: settings.json not found: $f"; exit 1 }

Copy-Item $f "$f.bak" -Force
$j = Get-Content $f -Raw | ConvertFrom-Json

# Ensure $j.hooks exists
if (-not $j.PSObject.Properties['hooks']) {
    $j | Add-Member -NotePropertyName hooks -NotePropertyValue ([pscustomobject]@{})
}

function Add-Hook($event, $cmd) {
    $entry = [pscustomobject]@{ type = 'command'; command = $cmd }
    if (-not $j.hooks.PSObject.Properties[$event] -or @($j.hooks.$event).Count -eq 0) {
        # event missing -> create array with one matcher-object holding the command
        $block = [pscustomobject]@{ hooks = @($entry) }
        if ($j.hooks.PSObject.Properties[$event]) {
            $j.hooks.$event = @($block)
        } else {
            $j.hooks | Add-Member -NotePropertyName $event -NotePropertyValue @($block)
        }
        return "added"
    }
    $existing = @($j.hooks.$event[0].hooks).command
    if ($existing -contains $cmd) { return "already present" }
    $j.hooks.$event[0].hooks = @($j.hooks.$event[0].hooks) + @($entry)
    return "added"
}

$bell = 'powershell -NoProfile -ExecutionPolicy Bypass -File "D:\Python\claudebar\hooks\claudebar-bell.ps1"'
$busy = 'powershell -NoProfile -ExecutionPolicy Bypass -File "D:\Python\claudebar\hooks\claudebar-busy.ps1"'

$rb = Add-Hook 'Stop' $bell
$ru = Add-Hook 'UserPromptSubmit' $busy
$rp = Add-Hook 'PostToolUse' $busy

$json = $j | ConvertTo-Json -Depth 100
[System.IO.File]::WriteAllText($f, $json, (New-Object System.Text.UTF8Encoding $false))
Write-Output "OK: bell(Stop) -> $rb; busy(UserPromptSubmit) -> $ru; keep-alive(PostToolUse) -> $rp"
