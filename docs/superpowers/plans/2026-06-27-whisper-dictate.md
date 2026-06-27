# whisper-dictate Container — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Минимальный Docker-контейнер вокруг faster-whisper на CUDA, дающий синхронный `POST /transcribe` для голосового ввода ClaudeBar.

**Architecture:** Один FastAPI-процесс. Модель грузится лениво на менее загруженную из 2×RTX 3060 (выбор через `nvidia-smi`), опционально выгружается по простою. `/transcribe` принимает аудио и возвращает `{"text"}` синхронно — никакого async/поллинга. Вся тяжёлая работа на faster-whisper.

**Tech Stack:** Python 3, faster-whisper (CTranslate2), FastAPI + uvicorn, Docker + nvidia runtime.

## Global Constraints

- Проект-папка: `D:\Python\whisper-dictate` (отдельный репозиторий, **НЕ** под GRACE).
- Модель по умолчанию: `dvislobokov/faster-whisper-large-v3-turbo-russian` (формат CT2, конвертация не нужна, MIT).
- Порт по умолчанию: `8771` (не пересекается с whisperx :8000).
- Контракт `/transcribe`: `multipart/form-data` (`file` + `hotwords`/`initial_prompt`/`language`) → `{"text": str, "duration": float}`. Синхронный. Пустая речь → `{"text": ""}`.
- env: `MODEL`, `DEVICE=cuda`, `COMPUTE_TYPE=float16`, `IDLE_UNLOAD_SECS=0`, `PORT=8771`.
- GPU: при загрузке модели выбирать `device_index` карты с максимумом свободной VRAM.
- Стиль: ponytail-full — минимум кода, faster-whisper делает всё. Чистый парсер `nvidia-smi` тестируем pytest; HTTP/ASR — curl-smoke.

---

### Task 1: Scaffold + выбор GPU (`pick_gpu`)

**Files:**
- Create: `D:\Python\whisper-dictate\gpu.py`
- Create: `D:\Python\whisper-dictate\requirements.txt`
- Create: `D:\Python\whisper-dictate\.gitignore`
- Test: `D:\Python\whisper-dictate\tests\test_gpu.py`

**Interfaces:**
- Produces: `pick_gpu_from(csv_text: str) -> int` (чистый парсер), `pick_gpu() -> int` (обёртка над `nvidia-smi`, fallback `0`).

- [ ] **Step 1: Создать папку проекта и git**

```bash
mkdir -p /d/Python/whisper-dictate/tests && cd /d/Python/whisper-dictate && git init
```

- [ ] **Step 2: Написать падающий тест парсера**

Создать `tests/test_gpu.py`:
```python
from gpu import pick_gpu_from


def test_picks_max_free():
    assert pick_gpu_from("0, 2048\n1, 9000\n") == 1


def test_first_on_tie():
    assert pick_gpu_from("0, 5000\n1, 5000\n") == 0


def test_empty_defaults_zero():
    assert pick_gpu_from("") == 0


def test_ignores_garbage_lines():
    assert pick_gpu_from("garbage\n0, 100\nbad,line\n1, 50\n") == 0
```

- [ ] **Step 3: Запустить — убедиться, что падает**

Run: `cd /d/Python/whisper-dictate && python -m pytest tests/test_gpu.py -q`
Expected: FAIL — `ModuleNotFoundError: No module named 'gpu'`

- [ ] **Step 4: Реализовать `gpu.py`**

```python
import subprocess


def pick_gpu_from(csv_text: str) -> int:
    """Разобрать вывод `nvidia-smi --query-gpu=index,memory.free
    --format=csv,noheader,nounits`. Вернуть индекс карты с максимумом
    свободных MiB. Пустой/мусорный ввод -> 0."""
    best_idx, best_free = 0, -1
    for line in csv_text.strip().splitlines():
        parts = [p.strip() for p in line.split(",")]
        if len(parts) < 2:
            continue
        try:
            idx, free = int(parts[0]), int(parts[1])
        except ValueError:
            continue
        if free > best_free:
            best_idx, best_free = idx, free
    return best_idx


def pick_gpu() -> int:
    try:
        out = subprocess.run(
            ["nvidia-smi", "--query-gpu=index,memory.free",
             "--format=csv,noheader,nounits"],
            capture_output=True, text=True, timeout=5,
        ).stdout
        return pick_gpu_from(out)
    except Exception:
        return 0
```

- [ ] **Step 5: Создать `requirements.txt`**

```
faster-whisper==1.0.3
fastapi==0.115.6
uvicorn[standard]==0.30.6
python-multipart==0.0.20
```

- [ ] **Step 6: Создать `.gitignore`**

```
__pycache__/
*.pyc
.pytest_cache/
cache/
*.wav
```

- [ ] **Step 7: Запустить тесты — зелёные**

Run: `cd /d/Python/whisper-dictate && python -m pytest tests/test_gpu.py -q`
Expected: PASS (4 passed)

- [ ] **Step 8: Commit**

```bash
cd /d/Python/whisper-dictate && git add -A && git commit -m "feat: scaffold + pick_gpu (least-loaded GPU parser)"
```

---

### Task 2: FastAPI-приложение (`/health`, `/transcribe`, жизненный цикл модели)

**Files:**
- Create: `D:\Python\whisper-dictate\app.py`

**Interfaces:**
- Consumes: `pick_gpu()` из `gpu.py`.
- Produces: ASGI-приложение `app`; маршруты `GET /health`, `POST /transcribe`.

- [ ] **Step 1: Реализовать `app.py`**

```python
import io
import os
import threading
import time

from fastapi import FastAPI, UploadFile, File, Form, HTTPException
from faster_whisper import WhisperModel

from gpu import pick_gpu

MODEL = os.getenv("MODEL", "dvislobokov/faster-whisper-large-v3-turbo-russian")
DEVICE = os.getenv("DEVICE", "cuda")
COMPUTE_TYPE = os.getenv("COMPUTE_TYPE", "float16")
IDLE_UNLOAD_SECS = int(os.getenv("IDLE_UNLOAD_SECS", "0"))

app = FastAPI(title="whisper-dictate")
_model = None
_device_idx = None
_last_used = 0.0
_lock = threading.Lock()


def _get_model():
    """Лениво загрузить модель на наименее загруженную карту. Потокобезопасно."""
    global _model, _device_idx, _last_used
    with _lock:
        if _model is None:
            _device_idx = pick_gpu() if DEVICE == "cuda" else 0
            _model = WhisperModel(MODEL, device=DEVICE,
                                  device_index=_device_idx,
                                  compute_type=COMPUTE_TYPE)
        _last_used = time.time()
        return _model


def _reaper():
    """Выгрузить модель после IDLE_UNLOAD_SECS простоя -> освободить VRAM.
    Следующий запрос перезагрузит её (и заново выберет карту)."""
    global _model
    while True:
        time.sleep(30)
        with _lock:
            if (_model is not None and IDLE_UNLOAD_SECS > 0
                    and time.time() - _last_used > IDLE_UNLOAD_SECS):
                _model = None


@app.on_event("startup")
def _startup():
    if IDLE_UNLOAD_SECS > 0:
        threading.Thread(target=_reaper, daemon=True).start()


@app.get("/health")
def health():
    return {
        "status": "ok",
        "model": MODEL,
        "device": f"{DEVICE}:{_device_idx}" if _device_idx is not None else DEVICE,
        "loaded": _model is not None,
    }


@app.post("/transcribe")
async def transcribe(
    file: UploadFile = File(...),
    language: str = Form("ru"),
    hotwords: str = Form(""),
    initial_prompt: str = Form(""),
):
    data = await file.read()
    if not data:
        raise HTTPException(status_code=400, detail="empty file")
    try:
        model = _get_model()
        segments, info = model.transcribe(
            io.BytesIO(data),
            language=language or "ru",
            hotwords=hotwords or None,
            initial_prompt=initial_prompt or None,
            beam_size=5,
        )
        text = "".join(s.text for s in segments).strip()
        return {"text": text, "duration": round(info.duration, 2)}
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))
```

- [ ] **Step 2: Проверить, что файл синтаксически корректен**

Run: `cd /d/Python/whisper-dictate && python -c "import ast; ast.parse(open('app.py').read()); print('ok')"`
Expected: `ok`

> Маршруты `/health` и `/transcribe` требуют GPU-рантайма и реальной модели — они проверяются curl-smoke в Task 3, а не юнит-тестом (faster-whisper локально без CUDA не запустить).

- [ ] **Step 3: Commit**

```bash
cd /d/Python/whisper-dictate && git add -A && git commit -m "feat: FastAPI app — /health, /transcribe, lazy GPU model"
```

---

### Task 3: Dockerfile + compose + сборка + GPU-smoke

**Files:**
- Create: `D:\Python\whisper-dictate\Dockerfile`
- Create: `D:\Python\whisper-dictate\docker-compose.yml`

**Interfaces:**
- Consumes: `app.py`, `gpu.py`, `requirements.txt`.
- Produces: образ `whisper-dictate:latest`, сервис на `:8771`.

- [ ] **Step 1: Создать `Dockerfile`**

> База — CUDA+cuDNN runtime (НЕ `python:3.11-slim`: faster-whisper/CTranslate2 на GPU требует cuBLAS+cuDNN). `nvidia-smi` доступен внутри контейнера через nvidia-runtime, поэтому `pick_gpu` работает.

```dockerfile
FROM nvidia/cuda:12.2.2-cudnn8-runtime-ubuntu22.04

RUN apt-get update && apt-get install -y --no-install-recommends \
        python3 python3-pip ffmpeg curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY requirements.txt .
RUN pip3 install --no-cache-dir -r requirements.txt
COPY gpu.py app.py ./

ENV PORT=8771
EXPOSE 8771
CMD ["sh", "-c", "uvicorn app:app --host 0.0.0.0 --port ${PORT}"]
```

- [ ] **Step 2: Создать `docker-compose.yml`**

```yaml
services:
  whisper-dictate:
    build: .
    image: whisper-dictate:latest
    container_name: whisper-dictate
    ports:
      - "8771:8771"
    environment:
      MODEL: dvislobokov/faster-whisper-large-v3-turbo-russian
      DEVICE: cuda
      COMPUTE_TYPE: float16
      IDLE_UNLOAD_SECS: "0"
      PORT: "8771"
    volumes:
      - hf-cache:/root/.cache/huggingface
    deploy:
      resources:
        reservations:
          devices:
            - driver: nvidia
              count: all
              capabilities: [gpu]
    mem_limit: 6g
    restart: unless-stopped

volumes:
  hf-cache:
```

- [ ] **Step 3: Собрать и поднять**

Run (PowerShell): `cd D:\Python\whisper-dictate; docker compose up -d --build`
Expected: образ собирается, контейнер `whisper-dictate` запущен. Первый старт тянет модель (~1.6 ГБ) в `hf-cache` — подождать загрузки.

- [ ] **Step 4: Проверить `/health`**

Run: `curl -s http://127.0.0.1:8771/health`
Expected: `{"status":"ok","model":"dvislobokov/faster-whisper-large-v3-turbo-russian","device":"cuda","loaded":false}` (loaded=false до первого запроса — модель ленивая).

- [ ] **Step 5: Сделать сэмпл-аудио (через уже работающий whisperx TTS)**

Run (PowerShell):
```powershell
curl.exe -s -X POST "http://127.0.0.1:8000/service/tts?backend=silero&voice=eugene&fmt=wav" -H "Content-Type: application/json" -d '{\"text\":\"Привет, это проверка голосового ввода в ClaudeBar.\"}' --output D:\Python\whisper-dictate\sample.wav
```
Expected: создан `sample.wav` (>0 байт).

- [ ] **Step 6: GPU-smoke `/transcribe`**

Run (PowerShell):
```powershell
curl.exe -s -X POST "http://127.0.0.1:8771/transcribe" -F "file=@D:\Python\whisper-dictate\sample.wav" -F "language=ru"
```
Expected: `{"text":"Привет, это проверка голосового ввода в ClaudeBar.","duration":<сек>}` (точные слова могут чуть отличаться).
Проверить `docker logs whisper-dictate` — модель встала на `cuda:<idx>` свободнейшей карты.

> **Если сборка/запуск падает на cuDNN/cuBLAS** (`Unable to load libcudnn*` / `libcublas`): несовместимость версии CTranslate2 с cuDNN базового образа. Поднять связку синхронно: либо база `nvidia/cuda:12.4.1-cudnn-runtime-ubuntu22.04` (cuDNN 9) + `faster-whisper` посвежее, либо оставить cudnn8 и закрепить `ctranslate2<4.5`. Менять обе стороны вместе, не по одной.

- [ ] **Step 7: Проверить hotwords (опционально, тот же сэмпл)**

Run (PowerShell):
```powershell
curl.exe -s -X POST "http://127.0.0.1:8771/transcribe" -F "file=@D:\Python\whisper-dictate\sample.wav" -F "language=ru" -F "hotwords=ClaudeBar" -F "initial_prompt=Используем приложение ClaudeBar."
```
Expected: запрос принят (200), в тексте «ClaudeBar» латиницей (а не «Клодбар»).

- [ ] **Step 8: Commit**

```bash
cd /d/Python/whisper-dictate && git add Dockerfile docker-compose.yml && git commit -m "feat: Dockerfile + compose (CUDA cudnn runtime, gpus all, hf-cache)"
```

---

### Task 4: README + финал

**Files:**
- Create: `D:\Python\whisper-dictate\README.md`

- [ ] **Step 1: Написать `README.md`**

```markdown
# whisper-dictate

Минимальный локальный STT-сервис для голосового ввода ClaudeBar.
faster-whisper (CTranslate2) на CUDA, дообученный на русском Whisper-turbo.

## Запуск
docker compose up -d --build

## API
- `POST /transcribe` — multipart: `file` (аудио) + опц. `language` (default ru),
  `hotwords`, `initial_prompt`. Ответ: `{"text": "...", "duration": <сек>}`.
- `GET /health` — `{status, model, device, loaded}`.

## Пример
curl -X POST http://127.0.0.1:8771/transcribe -F file=@sample.wav -F language=ru

## env
| Переменная | Default | Смысл |
|---|---|---|
| MODEL | dvislobokov/faster-whisper-large-v3-turbo-russian | id модели (CT2) |
| DEVICE | cuda | cuda / cpu |
| COMPUTE_TYPE | float16 | float16 / int8 |
| IDLE_UNLOAD_SECS | 0 | 0 = модель резидентна; >0 = выгрузка по простою (освобождает VRAM, +холодный старт) |
| PORT | 8771 | порт HTTP |

GPU выбирается автоматически — наименее загруженная карта (`nvidia-smi`) при загрузке модели.
```

- [ ] **Step 2: Commit**

```bash
cd /d/Python/whisper-dictate && git add README.md && git commit -m "docs: README"
```

---

## Self-Review

**Spec coverage (Часть 1):** faster-whisper CUDA ✓ (Task 2/3) · pick_gpu/nvidia-smi ✓ (Task 1) · синхронный `/transcribe` multipart→`{text}` ✓ (Task 2) · `/health` ✓ · модель dvislobokov CT2 ✓ · env MODEL/DEVICE/COMPUTE_TYPE/IDLE_UNLOAD_SECS/PORT ✓ · Dockerfile+compose (gpus all, restart, hf-cache volume, mem_limit) ✓ · pytest pick_gpu ✓ · curl-smoke ✓ · hotwords/initial_prompt ✓ (Task 3 Step 7). Поправка к спеку: база образа CUDA-cudnn, не slim (зафиксировано в Task 3 + риск-блок).

**Placeholder scan:** плейсхолдеров нет; весь код приведён целиком.

**Type consistency:** `pick_gpu_from`/`pick_gpu` совпадают между Task 1 и `app.py`; имена env и поля ответа (`text`,`duration`) согласованы со спеком и между задачами.
