# ClaudeBar hooks — «звоночек» завершения + индикатор работы

Два хука Claude Code сообщают панели ClaudeBar состояние проекта:

- `claudebar-bell.ps1` (событие **Stop**) — ИИ **закончила** работу: строка окна подсвечивается
  тёплой золотой полосой, плюс зажигается золотой квадрат сессии (мигает); гаснет при фокусе окна.
- `claudebar-busy.ps1` (событие **UserPromptSubmit**) — ИИ **начала** работу: на строке окна
  зажигается бирюзовый квадрат сессии (пульсирует). Каждая сессия = свой квадрат (Claude + Kimi считаются раздельно).

## Как это работает

1. По событию `Stop` (и опционально `Notification`) Claude Code запускает скрипт и передаёт
   ему на stdin JSON с полями `cwd` (папка проекта) и `session_id`.
2. Скрипт пишет файл `%APPDATA%\claudebar\signals\<session>.signal` с путём проекта.
3. ClaudeBar на каждом опросе (~1с) читает папку, берёт `basename(cwd)` как имя проекта
   и подсвечивает строки окон редактора с этим именем (`M-SIGNAL` → `M-RENDER`).
4. Когда окно проекта становится активным (любым способом), ClaudeBar удаляет файл-сигнал
   и снимает подсветку (`M-SIGNAL.reconcile`).

Сопоставление идёт **по имени проекта** (имя папки `cwd` = последний сегмент заголовка окна
VS Code/Cursor), а не по HWND — это устойчиво к перезапускам и интегрированному терминалу.

## Подключение

Проще всего — одной командой (добавит оба хука, бэкап + идемпотентно):

```
powershell -ExecutionPolicy Bypass -File "D:\Python\claudebar\hooks\install-bell-hook.ps1"
```

Вручную — два блока в `~/.claude/settings.json`:

```json
"Stop": [ { "hooks": [
  { "type": "command", "command": "powershell -NoProfile -ExecutionPolicy Bypass -File \"D:\\Python\\claudebar\\hooks\\claudebar-bell.ps1\"" }
] } ],
"UserPromptSubmit": [ { "hooks": [
  { "type": "command", "command": "powershell -NoProfile -ExecutionPolicy Bypass -File \"D:\\Python\\claudebar\\hooks\\claudebar-busy.ps1\"" }
] } ]
```

Bell-блок можно добавить и в `hooks.Notification` — подсветка на запросах подтверждения, не только на завершении.

## Индикатор работы (busy)

`UserPromptSubmit` пишет `%APPDATA%\claudebar\signals\<session>.busy` с `cwd`; пока файл свежий,
ClaudeBar рисует бирюзовый пульсирующий квадрат справа от названия проекта — по одному на каждую
активную сессию (две сессии в одном проекте → два квадрата). `PostToolUse` обновляет mtime `.busy`
на каждом инструменте (keep-alive) — квадрат держится всю длинную задачу. `Stop` удаляет `.busy`
и пишет `.signal` → квадрат становится золотым мигающим («готово»), гаснет при фокусе окна.

Если Claude убит без `Stop` (закрыт терминал и т.п.), ClaudeBar игнорирует `.busy` старше **90с**
по mtime — точки сами гаснут примерно через полторы минуты после остановки работы.

## Kimi Code CLI (тот же индикатор для другого агента)

Kimi Code CLI (`kimi`) поддерживает хуки как у Claude Code — конфиг `~/.kimi-code/config.toml`
(TOML `[[hooks]]`), payload на stdin с теми же полями `cwd`/`session_id`. Поэтому Kimi
переиспользует ТЕ ЖЕ скрипты `claudebar-busy.ps1` / `claudebar-bell.ps1`.

Подключение одной командой (бэкап `config.toml.bak` + идемпотентно):

```
powershell -ExecutionPolicy Bypass -File "D:\Python\claudebar\hooks\install-kimi-hook.ps1"
```

Скрипт добавит три блока: `UserPromptSubmit` и `PostToolUse` → `claudebar-busy.ps1`,
`Stop` → `claudebar-bell.ps1`. Каждая сессия пишет свой `<session_id>.busy`, поэтому ClaudeBar
показывает отдельный квадрат на каждый работающий терминал (Claude и Kimi вместе на одной строке).

На Windows Kimi запускает хук-команды через Git Bash — `powershell.exe` должен быть в PATH (обычно да).

## Ограничение

Подсветка работает для проектов, открытых в отслеживаемом окне (VS Code / Cursor).
Если Claude запущен во внешнем терминале и проект не открыт в редакторе — подсвечивать
нечего, сигнал ждёт появления окна.
