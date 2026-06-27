# Голосовой ввод в ClaudeBar — дизайн (v1)

> Статус: утверждён к реализации (brainstorming → этот спек → grace-plan).
> Дата: 2026-06-27.

## Цель

Заменить платный/нестабильный Blabby.AI: по горячей клавише записать голос, распознать локально
(дообученный на русском Whisper-turbo на GPU), почистить типовой мусор Whisper и вставить текст
`Ctrl+V` туда, где стоит курсор. Всё локально, бесплатно, приватно.

## Архитектура (два независимых артефакта)

1. **`whisper-dictate`** — отдельный минимальный Docker (новый проект `D:\Python\whisper-dictate`,
   как clfind). Синхронный HTTP вокруг faster-whisper на CUDA. Задаёт контракт, строится первым.
2. **ClaudeBar M-VOICE** — правки Rust/Win32-панели **под GRACE** (grace-plan/grace-execute):
   захват аудио, HTTP к контейнеру, чистка, вставка, индикатор, хоткей.

Граница между ними — HTTP-контракт `/transcribe`. Контейнер можно тестировать `curl`-ом без ClaudeBar;
ClaudeBar — мокая HTTP в юнит-тестах чистых функций.

---

## Часть 1. Контейнер `whisper-dictate`

### Стек
- База `python:3.11-slim`.
- Движок `faster-whisper` (CTranslate2). Никакого выравнивания/диаризации/TTS — чистый transcribe.
- Модель по умолчанию `dvislobokov/faster-whisper-large-v3-turbo-russian` (уже в формате CT2,
  faster-whisper грузит по id напрямую — конвертация не нужна; MIT; ~1.6 ГБ float16).
- HTTP: FastAPI + uvicorn (или stdlib `http.server` — решается на этапе плана; FastAPI проще для multipart).

### Выбор GPU (2×RTX 3060 12 ГБ)
При загрузке модели приложение опрашивает `nvidia-smi --query-gpu=index,memory.free
--format=csv,noheader,nounits`, парсит и берёт `device_index` карты с максимумом свободной памяти.
`faster-whisper` принимает `WhisperModel(..., device="cuda", device_index=N)`. Карта выбирается при
каждой загрузке модели (т.е. при `IDLE_UNLOAD_SECS>0` ещё и перебалансируется).

### API
- `POST /transcribe` — `multipart/form-data`:
  - `file` (обяз.) — аудио (WAV 16-bit PCM mono, но ffmpeg в образе примет любой формат).
  - `hotwords` (опц.) — строка терминов через пробел.
  - `initial_prompt` (опц.) — затравка-контекст.
  - `language` (опц., default `ru`).
  - **Синхронный ответ** `200 application/json`: `{"text": "...", "duration": <сек>}`.
    Пустая речь/тишина → `{"text": ""}`.
  - Ошибка → `4xx/5xx` + `{"error": "..."}`.
- `GET /health` → `{"status":"ok","model":"<id>","device":"cuda:<idx>","loaded":<bool>}`.

> Контракт сознательно НЕ OpenAI-совместимый и НЕ async (никаких `identifier`/`/task`-поллингов) —
> это и есть упрощение ради простого синхронного клиента в Rust.

### Управление моделью (env)
- `MODEL` (default `dvislobokov/faster-whisper-large-v3-turbo-russian`)
- `DEVICE` (default `cuda`), `COMPUTE_TYPE` (default `float16`)
- `IDLE_UNLOAD_SECS` (default `0` = резидент, мгновенная диктовка; `>0` = выгрузка модели после простоя,
  0 VRAM в покое + перебалансировка карты при следующем запросе, ценой холодного старта ~2–4 с).
- `PORT` (default `8771` — свободный, не пересекается с 8000 whisperx).

### Деплой
- `Dockerfile` + `docker-compose.yml`: `gpus: all` (доступ к обеим картам, выбор внутри),
  `restart: unless-stopped`, volume под HF-кэш модели (`~/.cache/huggingface`), лимит памяти.
- Smoke: `curl -F file=@sample.wav 'http://127.0.0.1:8771/transcribe?language=ru'`.

### Тесты
- Чистые: парсер вывода `nvidia-smi` → индекс свободнейшей карты (`pick_gpu`); сборка ответа.
- Интеграция: ручной `curl`-smoke (см. выше) + `/health`.

---

## Часть 2. ClaudeBar (GRACE)

### Конвейер (toggle-хоткей)
```
idle
  └─[хоткей Ctrl+Space]→ recording   (M-AUDIO пишет; нижняя полоса ЯРКАЯ)
       └─[хоткей ещё раз]→ stop
            └─ transcribing          (worker: WAV→POST /transcribe; полоса = бегущие точки)
                 └─ clean             (M-TRANSFORM: чистка мусора + опц. внешний transform)
                      └─ paste        (M-PASTE: clipboard + SendInput Ctrl+V в foreground)
                           └─ idle
```
Захват и HTTP — в worker-потоке, UI не морозим. Результат возвращается в UI-поток через
`PostMessage(WM_APP_VOICE_DONE)`; вставка делается в UI-потоке.

### Модули

| Модуль | Тип | Назначение | Чистые тесты |
|---|---|---|---|
| **M-AUDIO** | INTEGRATION | cpal: дефолтный микрофон → WAV 16-bit PCM mono в память; start/stop через канал | `encode_wav(samples,rate)` (заголовок/PCM) |
| **M-STT** | INTEGRATION | синхронный `POST /transcribe` (TcpStream-идиом из `sdaemon.rs`); собрать multipart, распарсить `{text}` | `build_multipart`, `parse_text`, `build_url(cfg)` |
| **M-TRANSFORM** | CORE | `clean_whisper(text)` — хардкод-чистка; `apply_vocab(text, map)` — пост-замена словаря; + конфиг-шов под внешний API (переформ./перевод, default off) | `clean_whisper`, `apply_vocab`, `select_mode`, `build_ext_request`/`parse_ext` |
| **M-PASTE** | UTILITY | clipboard (готовый `copy_to_clipboard`) + сохранить/вернуть прежний буфер + `SendInput Ctrl+V` | — (smoke) |
| **M-VOICE** | CORE_LOGIC | стейт-машина idle/recording/transcribing/pasting; спавн worker; `PostMessage` назад | переходы стейтов |
| **M-CONFIG** | + | новые ключи ini (ниже); `parse_hotkey(str)→(mods,vk)` | `parse_hotkey` |
| **M-MAIN** | + | `RegisterHotKey` (Ctrl+Space); `WM_HOTKEY`→`M-VOICE.toggle`; `WM_APP_VOICE_DONE`→paste; флаг записи в render | — (smoke) |
| **M-RENDER** | + | индикатор внизу окна (ниже) | — (smoke) |

### Индикатор (M-RENDER)
Полоса в **самой нижней части окна** панели (новая зона высотой ~4–6 px под последней строкой):
- **idle** — полоса скрыта/нейтральная.
- **recording** — полоса **ярко подсвечена** (насыщенный цвет, напр. ярко-красный/зелёный),
  визуально невозможно пропустить «идёт запись».
- **transcribing** — та же полоса показывает бегущие точки/пульс (переиспользуем механику busy-анимации:
  `anim_frame` + таймер `ID_ANIM_TIMER`), но не ярким «rec»-цветом.
Высота окна увеличивается на высоту полосы; раскладка строк сдвигается вверх на эту величину.

### Ключи `claudebar.ini` (M-CONFIG)
```
voice_hotkey   = Ctrl+Space                 # настраиваемая комбинация
whisper_url    = http://127.0.0.1:8771/transcribe
language       = ru
vocab          =                            # словарь пост-замены: клодбар=ClaudeBar; висперикс=WhisperX
hotwords       =                            # ОПЦ.: модельный hotwords (только для prompt-совместимой MODEL)
initial_prompt =                            # ОПЦ.: затравка-контекст (только для prompt-совместимой MODEL)
transform      = off                        # off | <имя внешнего режима>  (v1: только off)
transform_url  =                            # внешний API для переформулирования/перевода (фаза D)
```

### Кастомный словарь (паритет с Blabby)
**Основной механизм — пост-замена в M-TRANSFORM** (детерминированно, не зависит от модели). Словарь
`wrong→right` в ini (напр. `клодбар=ClaudeBar`, `висперикс=WhisperX`); после распознавания текст
прогоняется через замены (регистронезависимо, по границам слова). Причина: дефолтная дообученная модель
`dvislobokov/...-russian` **не держит** `hotwords`/`initial_prompt` (см. README контейнера и риск-8 —
любой prompt ломает её в мусор/пустоту), зато плейн-русский у неё лучший.

Модельный `hotwords`/`initial_prompt` остаётся **опцией**: ключи ini прокидываются M-STT в `/transcribe`,
но включать их стоит только при смене `MODEL` контейнера на prompt-совместимую (напр. базовую
`openai/whisper-large-v3-turbo`). По умолчанию не шлём.

### Чистка мусора Whisper (M-TRANSFORM v1, всегда вкл)
`clean_whisper(text)`:
- `trim`, схлопывание повторных пробелов, заглавная после `.?!`.
- Выкидывание известных галлюцинаций на тишине/шуме, если ими является весь вывод или хвостовая строка:
  «Продолжение следует…», «Субтитры подготовил…», «Субтитры сделал…», «Спасибо за просмотр»,
  «Редактор субтитров …», «ПОДПИШИСЬ …», «[музыка]», «Дякую за перегляд» и т.п. (список-константа, расширяемый).
- Схлопывание непосредственных повторов слова/фразы (whisper-залипания).
- Пустой результат → ничего не вставляем.

### Обработка ошибок
- Контейнер недоступен (`/transcribe` не отвечает) → стейт обратно в idle, индикатор гаснет,
  короткий лог-маркер; не падаем, не вставляем.
- Пустой `text` → idle без вставки.
- Хоткей нажат во время transcribing → игнор (или отмена) — не запускаем второй захват.
- Если фокус на момент хоткея был на самой панели — запомнить целевой HWND до записи; при необходимости
  поднять его `AttachThreadInput`-трюком (`activate.rs`) перед вставкой.
- M-PASTE сохраняет прежнее содержимое буфера обмена и возвращает его после вставки.

---

## Зависимости

- **Rust:** новая — только `cpal` (де-факто аудио-захват для Rust; чистый windows-rs, без C-компилятора —
  проверить сборку под GNU-тулчейном **первым шагом**, по образцу risk-проверки rusqlite; фолбэк — ручной WASAPI).
  HTTP — **без новой зависимости** (самописный поверх `TcpStream`, как `sdaemon.rs`; localhost http, TLS не нужен).
  Клипборд — **готов** (`copy_to_clipboard`).
- **Контейнер (Python):** `faster-whisper`, `fastapi`, `uvicorn`, `python-multipart`. Лицензии: модель MIT.

## Тестирование / верификация
- Чистые функции (cargo test, GRACE-разметка): `encode_wav`, `build_multipart`, `parse_text`,
  `build_url`, `clean_whisper`, `parse_hotkey`, `select_mode`; контейнер — `pick_gpu`.
- Ручной smoke: контейнер `curl`; ClaudeBar — запись→распознавание→вставка end-to-end; индикатор;
  затирание/возврат буфера; недоступный контейнер.

## Риски
1. **Сборка `cpal` под windows-gnu** — проверить до всего остального; фолбэк ручной WASAPI.
2. **Латентность GPU** — резидентная модель = sub-second; при `IDLE_UNLOAD_SECS>0` первый запрос после
   простоя +2–4 с (холодная загрузка).
3. **Конфликт хоткея** `Ctrl+Space` (IME/автодополнение в редакторах) — настраиваемый в ini.
4. **Фокус/вставка** — панель `WS_EX_NOACTIVATE`, фокус не крадётся; целевой HWND фиксируем на старте записи.
5. **Затирание буфера обмена** — сохранить/вернуть в M-PASTE.
6. **Микрофон** — дефолтное устройство ввода; выбор устройства — позже (env/ini).
7. **Деление GPU** с другими контейнерами (docling/cosyvoice) — смягчается выбором свободнейшей карты
   и опциональной выгрузкой по простою.
8. **Модель не держит prompt/hotwords** (подтверждено smoke 2026-06-27): дефолтная `dvislobokov`-turbo
   на любой `hotwords`/`initial_prompt` выдаёт мусор/пустоту. Снято: словарь делаем пост-заменой
   (`apply_vocab`), модельный hotwords — опция при смене `MODEL` на базовую turbo.

## Порядок реализации
1. **Контейнер `whisper-dictate`** (фундамент, контракт): faster-whisper + `pick_gpu` + `/transcribe` + `/health`
   + Dockerfile/compose; `curl`-smoke зелёный.
2. **ClaudeBar (GRACE), фазами:**
   - **A** — M-CONFIG (ключи + `parse_hotkey`) + M-AUDIO (захват → WAV; smoke: запись в файл).
   - **B** — M-STT (POST→`{text}`, тесты на реальных сэмплах от контейнера).
   - **C** — M-VOICE + M-PASTE + M-MAIN (хоткей) + M-RENDER (нижняя полоса) = **рабочий MVP диктовки**.
   - **D** (позже) — M-TRANSFORM внешний transform (переформулирование/перевод) + UI настроек/словаря.

## Вне области v1 (на потом)
- AI-режимы Blabby (полноценные кастомные инструкции, «деловое письмо» и т.п.).
- UI настроек словаря/режимов внутри панели (v1 — правка ini руками).
- Выбор микрофона в UI; стриминговое распознавание по ходу речи.
