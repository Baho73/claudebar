# ClaudeBar hooks — «звоночек» завершения + индикатор работы

Два хука Claude Code сообщают панели ClaudeBar состояние проекта:

- `claudebar-bell.ps1` (событие **Stop**) — ИИ **закончила** работу: строка окна подсвечивается
  тёплой золотой полосой; подсветка гаснет, когда окно проекта получает фокус.
- `claudebar-busy.ps1` (событие **UserPromptSubmit**) — ИИ **начала** работу: на строке окна
  бегут точки «...». Stop-хук удаляет busy-маркер, и точки сменяются звоночком.

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

`UserPromptSubmit` пишет `%APPDATA%\claudebar\signals\<session>.busy` с `cwd`; пока файл есть,
ClaudeBar анимирует «...» на строке проекта. `Stop` удаляет `.busy` (и пишет `.signal` звоночка).
Если Claude убит без `Stop`, ClaudeBar игнорирует `.busy` старше ~600с (по mtime), чтобы точки не висели вечно.

## Ограничение

Подсветка работает для проектов, открытых в отслеживаемом окне (VS Code / Cursor).
Если Claude запущен во внешнем терминале и проект не открыт в редакторе — подсвечивать
нечего, сигнал ждёт появления окна.
