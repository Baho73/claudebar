# start-whisper.ps1 — поднять Docker Desktop и контейнер whisper-dictate при входе в систему.
#
# Зачем: Docker Desktop уже стоит в автозапуске (HKCU\...\Run), а контейнер помечен
# restart: unless-stopped — но этой пары не хватает в двух случаях:
#   1) контейнер остановили руками (`docker stop`) — unless-stopped его НЕ поднимет;
#   2) движок Docker поднимается 1-3 минуты, и всё это время диктовка недоступна молча.
# Скрипт ждёт готовности движка и делает идемпотентный `compose up -d` (если всё уже
# работает — no-op). Запускается из автозагрузки через start-whisper.vbs (без окна консоли).
#
# Ручной запуск (PowerShell):  powershell -ExecutionPolicy Bypass -File tools\start-whisper.ps1

# Именно 'Continue': docker compose пишет прогресс («Container ... Running») в stderr, а при 'Stop'
# перенаправленный stderr нативной команды превращается в терминирующую ошибку. Успех проверяем
# по $LASTEXITCODE, а не по отсутствию вывода в stderr.
$ErrorActionPreference = 'Continue'

# --- параметры ---
$ComposeFile = 'D:\Python\whisper-dictate\docker-compose.yml'
$DockerExe   = 'C:\Program Files\Docker\Docker\Docker Desktop.exe'
$LogFile     = Join-Path $env:APPDATA 'claudebar\start-whisper.log'
$WaitSeconds = 300   # сколько ждём готовности движка (холодный старт WSL бывает долгим)

function Write-Log($msg) {
    $line = "[{0}] {1}" -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $msg
    $dir = Split-Path $LogFile -Parent
    if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    Add-Content -Path $LogFile -Value $line -Encoding UTF8
}

try {
    Write-Log 'старт'

    if (-not (Test-Path $ComposeFile)) {
        Write-Log "НЕТ compose-файла: $ComposeFile — выходим"
        exit 1
    }

    # 1. Docker Desktop запущен? (автозапуск обычно уже сделал это — тогда просто идём дальше)
    if (-not (Get-Process 'Docker Desktop' -ErrorAction SilentlyContinue)) {
        if (Test-Path $DockerExe) {
            Write-Log 'Docker Desktop не запущен — запускаем'
            Start-Process -FilePath $DockerExe -WindowStyle Minimized
        } else {
            Write-Log "НЕ найден $DockerExe — выходим"
            exit 1
        }
    }

    # 2. Ждём готовности движка: `docker info` начинает отвечать только когда демон поднялся.
    $deadline = (Get-Date).AddSeconds($WaitSeconds)
    $ready = $false
    while ((Get-Date) -lt $deadline) {
        docker info 2>&1 | Out-Null
        if ($LASTEXITCODE -eq 0) { $ready = $true; break }
        Start-Sleep -Seconds 5
    }
    if (-not $ready) {
        Write-Log "движок не поднялся за ${WaitSeconds}с — выходим"
        exit 1
    }
    Write-Log 'движок готов'

    # 3. Идемпотентно поднять сервисы (уже работают -> no-op; остановлены руками -> стартуют).
    $out = docker compose -f $ComposeFile up -d 2>&1
    Write-Log ("compose up -d (код {0}): {1}" -f $LASTEXITCODE, ($out -join ' | '))

    # 4. Дождаться /health — чтобы в логе было видно, что модель реально загрузилась.
    $port = 18771
    $healthDeadline = (Get-Date).AddSeconds(180)
    while ((Get-Date) -lt $healthDeadline) {
        try {
            $r = Invoke-WebRequest -Uri "http://127.0.0.1:$port/health" -TimeoutSec 5 -UseBasicParsing
            Write-Log "health: $($r.Content)"
            exit 0
        } catch {
            Start-Sleep -Seconds 5
        }
    }
    Write-Log 'health не ответил за 180с (контейнер поднят, но модель ещё грузится?)'
} catch {
    Write-Log "ОШИБКА: $($_.Exception.Message)"
    exit 1
}
