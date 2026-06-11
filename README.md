# ClaudeBar

A tiny always-on-top switcher for your open editor windows — built for the moment you have **a dozen Claude Code sessions** running in different VS Code / Cursor windows and can no longer tell them apart.

It shows a compact vertical list of your project windows. Click one to jump to it. Tag each project with a **color** and a **free-text label** (model, status, whatever) so you always know which is which.

Native Windows `.exe`, ~280 KB, written in Rust. No Python, no .NET, no runtime to install — one file.

```
┌────────────────────────┐
│ ≡ ClaudeBar          ✕ │
│ ▌ ConstructMan    opus │   ← color swatch · project · label
│ ▌ Test_2026.05  sonnet │   ← active window is highlighted
│ ▌ hh_answer      review│
└────────────────────────┘
```

## Why

When you run many Claude Code sessions, each lives in its own editor window. The taskbar and Alt-Tab show near-identical entries, and you waste time hunting for the right one. ClaudeBar gives every project a stable spot, a color, and a label, and switches to it in one click.

## Features

- Always-on-top vertical bar, lists every window of a known editor/office process (VS Code, Cursor, Word, Excel, MS Project).
- Groups by **project name**, not the active file — the row stays put when you switch files.
- **Left-click** a row → switch to that window (restores it if minimized).
- **Right-click** a row → pick a color (8 presets) and set a label.
- Color + label are bound to the **project name**, so they survive switching files and reopening the window. Stored in `claudebar.ini` next to the exe.
- Drag the panel by its header; position is remembered. `✕` to close.
- Auto-refreshes about once a second — new windows appear, closed ones disappear.

## Install

**Option A — download.** Grab `claudebar.exe` from [Releases](https://github.com/Baho73/claudebar/releases), drop it anywhere, run it.

**Option B — build from source** (see below).

> **Antivirus note.** ClaudeBar is an unsigned binary that enumerates windows and changes focus (`SetForegroundWindow`). Some antivirus engines (e.g. Kaspersky) flag that behavior heuristically and may quarantine it. If the exe vanishes or won't start, add it (or its folder) to your AV's trusted/exclusions list. The source is right here — read it, build it yourself if you prefer.

## Usage

- **Left-click** a row — focus that window.
- **Right-click** a row — context menu:
  - choose one of 8 colors for the project,
  - **Метка… / Label…** — type a short label (model, task, status),
  - **Убрать метку / Clear label**.
- **Drag** the header strip to move the panel.
- **✕** in the header — quit.

## How it finds windows

ClaudeBar lists top-level visible windows that belong to a known editor/office **process**. Built-in set:

```
code.exe    → VS Code
cursor.exe  → Cursor
winword.exe → Word
excel.exe   → Excel
winproj.exe → MS Project
```

The **project name** is extracted from the window title per app: for VS Code / Cursor it is the segment just before the ` - Visual Studio Code` / ` - Cursor` suffix (titles look like `file.rs - ProjectName - Visual Studio Code`); for Office apps it is the document name.

The tracked set is built in (matched by process name); there is no user-editable pattern list yet — see BACKLOG.

## Config file (`claudebar.ini`)

Created automatically next to the exe. Plain text:

```
# claudebar config
pos=1570,40
c=Excel
p=ConstructMan	3	opus
p=Test_2026.05.28	1	sonnet
```

- `pos=X,Y` — panel position.
- `c=<block>` — collapsed section (by app block name).
- `re=<block>` — section with the "recent" sub-block expanded.
- `p=<project>\t<colorIndex 0-7>\t<label>` — per-project settings (tab-separated; color `-1` means auto).

## Build from source

Needs Rust. Without Visual Studio, use the self-contained GNU toolchain:

```powershell
# install rustup with the GNU toolchain
rustup-init.exe -y --default-host x86_64-pc-windows-gnu --default-toolchain stable

# build
cargo build --release
# -> target\release\claudebar.exe
```

Single dependency: the official [`windows`](https://crates.io/crates/windows) crate (Win32 bindings).

## Limitations

- Switches between **windows**. If you run several Claude Code sessions in tabs inside one editor window, they can't be told apart by window — one session per window is the supported setup.
- Windows only (Win32).

## License

MIT — see [LICENSE](LICENSE).

---

## По-русски, коротко

Крошечная всегда-поверх панель для переключения между окнами редакторов (VS Code / Cursor), когда открыто много сессий Claude Code и они сливаются. Список проектов в углу экрана: левый клик — перейти в окно, правый клик — задать цвет и метку (модель, статус). Цвет и метка привязаны к имени проекта и не слетают при смене файла и перезапуске окна. Нативный `.exe` ~280 КБ на Rust, без зависимостей.

Запуск может блокировать антивирус (Касперский считает подозрительным неподписанный exe, который переключает фокус окон) — добавь exe или папку в исключения.
