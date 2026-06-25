// FILE: src/render.rs
// VERSION: 1.11.0
// START_MODULE_CONTRACT
//   PURPOSE: Построение строк-секций и отрисовка панели (GDI, двойной буфер) с группировкой по приложению.
//   SCOPE: геометрия/цвета, Row, build_rows, paint (секции+иконки+окна+недавние+подсветка звоночка), resize, row_at.
//   DEPENDS: M-CONFIG (палитра, цвета/метки, свёрнутость), M-WINENUM (WinItem), M-RECENT (RecentDoc), M-ICON (иконки секций), App.bell (набор звенящих имён проектов)
//   LINKS: M-RENDER
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   Row, Zone, W, HEAD, ROW - модель строк, зоны клика и геометрия
//   build_rows        - сгруппировать окна в строки секций с учётом свёрнутости и ручного порядка
//   paint             - отрисовать строки (секции + окна + ✕/ручки + подсветка drag)
//   resize            - подогнать высоту окна под число строк
//   row_at            - индекс строки по координате Y
//   hit_test          - (строка, Zone) по координатам клика (с учётом режима reorder)
//   folder_project / search_color_for / search_result_rows - поиск (Phase-12): имя проекта из пути, цвет открытой совпавшей папки, строки «Найдено ещё» по закрытым
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.11.0 - Phase-12 polish: поле поиска постоянно в шапке (не по клику 🔍); «≡» слева — ручка drag; индикатор совпадения = иконка в цветной рамке (draw_framed_icon, 🟡 BM25 / 🔵 dense) вместо тонкой полосы; иконки в «Найдено ещё» (icon::path_icon).
//   v1.10.0 - Phase-12 Step 4: глиф «🔍» в шапке; подсветка совпавших открытых папок цветной полосой (🟡 BM25 / 🔵 dense); Row::SearchHeader/SearchResult + блок «Найдено ещё»; чистые folder_project/search_color_for/search_result_rows.
//   v1.9.0 - Phase-9 Step 3: глиф «⚙» (настройки) в шапке слева от «✕»; pub HEAD_BTN_W — общая ширина кнопок шапки.
//   v1.8.0 - fix: перетаскивание необнаружимо. В режиме reorder хватается вся строка (не только ручка), шапка показывает подсказку «↕ Порядок».
//   v1.7.0 - Phase-8 Step 2: ручной порядок в build_rows, Zone::DragHandle, ручки и подсветка drag.
//   v1.6.0 - Phase-7 Step 3: Row::RecentMore — «показать все» недавних сверх 6 (VISIBLE_RECENT).
//   v1.5.0 - Phase-6 Step 1: Zone {Body, Close}, hit_test, отрисовка ✕ на hover строки окна.
//   v1.4.0 - Phase-5 Step 2: иконка приложения в заголовке секции (M-ICON), сдвиг названия.
//   v1.3.0 - Phase-4 Step 2: подсветка «звенящих» строк по набору App.bell (имя проекта из сигнала).
//   v1.2.0 - Phase-2 Step 2: секции по приложению, крыжик сворачивания, build_rows.
//   v1.0.0 - Выделено из монолита (Phase-1, Step 4).
// END_CHANGE_SUMMARY

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{AppDef, Config, PALETTE};
use crate::icon;
use crate::recent::RecentDoc;
use crate::search::{Color, FolderHit};
use crate::win_enum::WinItem;
use crate::App;
use std::collections::HashSet;

// геометрия
pub const W: i32 = 252;
pub const HEAD: i32 = 24;
pub const ROW: i32 = 30;
const SWATCH: i32 = 14;
const CLOSE_W: i32 = 24; // ширина правой зоны кнопки ✕ на строке окна
const VISIBLE_RECENT: usize = 6; // сколько недавних показывать до «показать все»
pub const HEAD_BTN_W: i32 = 24; // ширина кнопки в шапке (✕ закрыть панель, ⚙ настройки)

// Строка панели: заголовок секции приложения или окно внутри секции.
#[derive(Clone, Copy)]
pub enum Row {
    Section { app: usize },
    Window { idx: usize }, // индекс в App.items
    RecentHeader { app: usize }, // под-заголовок «Недавние»
    Recent { ridx: usize }, // индекс в App.recent
    RecentMore { app: usize }, // строка «показать все / свернуть» недавних
    SearchHeader, // под-заголовок «Найдено ещё» (закрытые совпадения поиска) — Phase-12
    SearchResult { hit: usize }, // индекс в App.search_hits (закрытая папка-совпадение) — Phase-12
}

// Зона клика внутри строки.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Zone {
    Body,       // основное тело строки (активация / открытие / сворачивание)
    Close,      // правая зона ✕ на строке окна
    DragHandle, // правая зона ручки drag в режиме reorder (Section/Window)
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}
const C_BG: (u8, u8, u8) = (16, 21, 38);
const C_HEAD: (u8, u8, u8) = (26, 34, 58);
const C_SECTION: (u8, u8, u8) = (22, 29, 52);
const C_TXT: (u8, u8, u8) = (223, 230, 243);
const C_DIM: (u8, u8, u8) = (150, 165, 200);
const C_ACTIVE: (u8, u8, u8) = (34, 52, 96);
const C_HOVER: (u8, u8, u8) = (24, 32, 60);
const C_BORDER: (u8, u8, u8) = (40, 54, 90);
const C_REC: (u8, u8, u8) = (170, 182, 206);
const C_BELL: (u8, u8, u8) = (70, 56, 22); // фон строки со «звоночком» — тёплое тёмное золото
const C_BELL_BAR: (u8, u8, u8) = (246, 189, 22); // левая полоса-индикатор «звоночка»
const C_SRCH_BM25: (u8, u8, u8) = (245, 200, 40); // жёлтая полоса поиска: совпадение по словам (BM25)
const C_SRCH_DENSE: (u8, u8, u8) = (91, 143, 249); // синяя полоса поиска: совпадение по смыслу (dense)

unsafe fn dt(hdc: HDC, s: &str, mut r: RECT, fmt: DRAW_TEXT_FORMAT) {
    if s.is_empty() {
        return;
    }
    let mut b: Vec<u16> = s.encode_utf16().collect();
    DrawTextW(hdc, &mut b, &mut r, fmt | DT_NOPREFIX);
}

unsafe fn fill(hdc: HDC, r: RECT, c: (u8, u8, u8)) {
    let br = CreateSolidBrush(rgb(c.0, c.1, c.2));
    FillRect(hdc, &r, br);
    let _ = DeleteObject(br);
}

// ручка перетаскивания: 6 точек (2 столбца × 3 ряда) в правой зоне строки
unsafe fn draw_handle(hdc: HDC, rx: i32, top: i32) {
    let y0 = top + (ROW - 12) / 2;
    for col in 0..2 {
        for row in 0..3 {
            let x = rx + col * 5;
            let y = y0 + row * 5;
            fill(hdc, RECT { left: x, top: y, right: x + 2, bottom: y + 2 }, C_DIM);
        }
    }
}

// иконка 16x16 в цветной рамке (2px) — индикатор совпадения поиска (🟡/🔵)
unsafe fn draw_framed_icon(hdc: HDC, x: i32, top: i32, icon: HICON, c: (u8, u8, u8)) {
    let fy = top + (ROW - 20) / 2;
    fill(hdc, RECT { left: x, top: fy, right: x + 20, bottom: fy + 20 }, c);
    let _ = DrawIconEx(hdc, x + 2, fy + 2, icon, 16, 16, 0, HBRUSH(std::ptr::null_mut()), DI_NORMAL);
}

// START_CONTRACT: build_rows
//   PURPOSE: Сгруппировать окна по приложению в строки секций, скрыть содержимое свёрнутых секций.
//   INPUTS: { items: &[WinItem]; apps: &[AppDef]; cfg: &Config - состояние свёрнутости }
//   OUTPUTS: { Vec<Row> - заголовки секций и (если развёрнуто) строки окон }
//   SIDE_EFFECTS: none
// END_CONTRACT: build_rows
pub fn build_rows(items: &[WinItem], recent: &[RecentDoc], apps: &[AppDef], cfg: &Config) -> Vec<Row> {
    let mut win_by_app: Vec<Vec<usize>> = vec![Vec::new(); apps.len()];
    for (i, it) in items.iter().enumerate() {
        if it.app < apps.len() {
            win_by_app[it.app].push(i);
        }
    }
    // ручной порядок окон внутри секции (неизвестные — после, по имени)
    for a in 0..apps.len() {
        let block = &apps[a].block;
        win_by_app[a].sort_by(|&x, &y| {
            match (cfg.window_rank(block, &items[x].name), cfg.window_rank(block, &items[y].name)) {
                (Some(p), Some(q)) => p.cmp(&q),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                // без ручной позиции: recent -> по ordinal (позже открыто -> ниже), иначе по имени — Phase-16
                (None, None) => {
                    if cfg.is_sort_recent() {
                        items[x].ordinal.cmp(&items[y].ordinal)
                    } else {
                        items[x].name.cmp(&items[y].name)
                    }
                }
            }
        });
    }
    let mut rec_by_app: Vec<Vec<usize>> = vec![Vec::new(); apps.len()];
    for (i, d) in recent.iter().enumerate() {
        if d.app < apps.len() {
            rec_by_app[d.app].push(i);
        }
    }
    let mut rows = Vec::new();
    // START_BLOCK_GROUP_SECTIONS
    for a in cfg.section_index_order(apps) {
        if win_by_app[a].is_empty() && rec_by_app[a].is_empty() {
            continue;
        }
        rows.push(Row::Section { app: a });
        if cfg.is_collapsed(&apps[a].block) {
            continue;
        }
        for &idx in &win_by_app[a] {
            rows.push(Row::Window { idx });
        }
        if !rec_by_app[a].is_empty() {
            rows.push(Row::RecentHeader { app: a });
            if cfg.is_recent_open(&apps[a].block) {
                // START_BLOCK_RECENT_VISIBLE
                let recs = &rec_by_app[a];
                let showall = cfg.is_showall(&apps[a].block);
                let visible = if showall { recs.len() } else { recs.len().min(VISIBLE_RECENT) };
                for &ridx in &recs[..visible] {
                    rows.push(Row::Recent { ridx });
                }
                if recs.len() > VISIBLE_RECENT {
                    rows.push(Row::RecentMore { app: a });
                }
                // END_BLOCK_RECENT_VISIBLE
            }
        }
    }
    // END_BLOCK_GROUP_SECTIONS
    rows
}

// START_CONTRACT: paint
//   PURPOSE: Отрисовать панель (шапка, секции, окна) из состояния App.
//   INPUTS: { hwnd: HWND; app: &App }
//   OUTPUTS: { () }
//   SIDE_EFFECTS: рисует на экране (GDI, двойной буфер)
// END_CONTRACT: paint
pub unsafe fn paint(hwnd: HWND, app: &App) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let w = rc.right;
    let h = rc.bottom;

    let mem = CreateCompatibleDC(hdc);
    let bmp = CreateCompatibleBitmap(hdc, w, h);
    let old = SelectObject(mem, bmp);

    fill(mem, RECT { left: 0, top: 0, right: w, bottom: h }, C_BG);
    let head_bg = if app.reorder { C_BELL } else { C_HEAD };
    fill(mem, RECT { left: 0, top: 0, right: w, bottom: HEAD }, head_bg);
    SetBkMode(mem, TRANSPARENT);

    // шапка: в режиме reorder — подсказка, иначе заголовок + кнопка закрытия панели
    SelectObject(mem, app.font_small);
    if app.reorder {
        SetTextColor(mem, rgb(C_BELL_BAR.0, C_BELL_BAR.1, C_BELL_BAR.2));
        dt(mem, "↕ Порядок — тащите строки, ПКМ — выход", RECT { left: 10, top: 0, right: w - 8, bottom: HEAD }, DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS);
    } else {
        SetTextColor(mem, rgb(C_DIM.0, C_DIM.1, C_DIM.2));
        dt(mem, "≡", RECT { left: 4, top: 0, right: 18, bottom: HEAD }, DT_SINGLELINE | DT_VCENTER | DT_LEFT);
        // окошко поиска (child EDIT) занимает середину шапки [18 .. w-2*HEAD_BTN_W]
        dt(mem, "⚙", RECT { left: w - 2 * HEAD_BTN_W, top: 0, right: w - HEAD_BTN_W, bottom: HEAD }, DT_SINGLELINE | DT_VCENTER | DT_CENTER);
        dt(mem, "✕", RECT { left: w - HEAD_BTN_W, top: 0, right: w, bottom: HEAD }, DT_SINGLELINE | DT_VCENTER | DT_CENTER);
    }

    if app.rows.is_empty() {
        SetTextColor(mem, rgb(C_DIM.0, C_DIM.1, C_DIM.2));
        dt(mem, "нет окон", RECT { left: 10, top: HEAD, right: w - 10, bottom: HEAD + ROW }, DT_SINGLELINE | DT_VCENTER | DT_LEFT);
    }

    let fg = GetForegroundWindow();
    for (i, row) in app.rows.iter().enumerate() {
        let top = HEAD + i as i32 * ROW;
        let full = RECT { left: 0, top, right: w, bottom: top + ROW };
        match row {
            Row::Section { app: a } => {
                fill(mem, full, C_SECTION);
                if app.hover == i as i32 {
                    fill(mem, full, C_HOVER);
                }
                let def: &AppDef = &app.config.apps[*a];
                let tri = if app.config.is_collapsed(&def.block) { "▶" } else { "▼" };
                SelectObject(mem, app.font_small);
                SetTextColor(mem, rgb(C_DIM.0, C_DIM.1, C_DIM.2));
                dt(mem, tri, RECT { left: 8, top, right: 24, bottom: top + ROW }, DT_SINGLELINE | DT_VCENTER | DT_LEFT);
                // START_BLOCK_SECTION_ICON
                let mut text_left = 26;
                if let Some(sample) = app.items.iter().find(|it| it.app == *a).map(|it| it.hwnd) {
                    if let Some(hicon) = icon::section_icon(*a, sample) {
                        let iy = top + (ROW - 16) / 2;
                        let _ = DrawIconEx(mem, 26, iy, hicon, 16, 16, 0, HBRUSH(std::ptr::null_mut()), DI_NORMAL);
                        text_left = 48;
                    }
                }
                // END_BLOCK_SECTION_ICON
                SelectObject(mem, app.font_main);
                SetTextColor(mem, rgb(C_TXT.0, C_TXT.1, C_TXT.2));
                dt(mem, &def.block, RECT { left: text_left, top, right: w - 36, bottom: top + ROW }, DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS);
                if app.reorder {
                    draw_handle(mem, w - 18, top);
                } else {
                    let cnt = app.items.iter().filter(|it| it.app == *a).count();
                    SelectObject(mem, app.font_small);
                    SetTextColor(mem, rgb(C_DIM.0, C_DIM.1, C_DIM.2));
                    dt(mem, &cnt.to_string(), RECT { left: w - 34, top, right: w - 12, bottom: top + ROW }, DT_SINGLELINE | DT_VCENTER | DT_RIGHT);
                }
            }
            Row::Window { idx } => {
                let it: &WinItem = &app.items[*idx];
                let id_key = it.path.as_deref().unwrap_or(&it.name); // идентичность: путь, иначе имя (Phase-15)
                // START_BLOCK_ROW_BG_WINDOW
                // звоночек: окно с путём — точный матч по cwd; без пути — по basename (Phase-15)
                let belling = match it.path.as_deref() {
                    Some(p) => app.bell_paths.contains(&p.to_lowercase()),
                    None => app.bell.contains(&it.name.to_lowercase()),
                };
                if it.hwnd == fg {
                    fill(mem, full, C_ACTIVE);
                } else if belling {
                    // «звоночек»: ИИ закончила работу в этом проекте — тёплая подсветка + левая полоса
                    fill(mem, full, C_BELL);
                    fill(mem, RECT { left: 0, top, right: 3, bottom: top + ROW }, C_BELL_BAR);
                } else if app.hover == i as i32 {
                    fill(mem, full, C_HOVER);
                }
                // END_BLOCK_ROW_BG_WINDOW
                // подсветка поиска: иконка приложения в цветной рамке слева (🟡/🔵)
                if let Some(scol) = search_color_for(&app.search_hits, &it.name.to_lowercase()) {
                    let fc = match scol { Color::Bm25 => C_SRCH_BM25, Color::Dense => C_SRCH_DENSE };
                    if let Some(hicon) = icon::section_icon(it.app, it.hwnd) {
                        draw_framed_icon(mem, 0, top, hicon, fc);
                    } else {
                        fill(mem, RECT { left: 0, top, right: 5, bottom: top + ROW }, fc);
                    }
                }
                // цветная плашка (с отступом — окна вложены в секцию)
                let cy = top + (ROW - SWATCH) / 2;
                let (_, r, g, b) = PALETTE[app.config.color_idx_for(id_key, &it.name)];
                fill(mem, RECT { left: 22, top: cy, right: 22 + SWATCH, bottom: cy + SWATCH }, (r, g, b)); // +2px зазор от иконки-рамки поиска
                // имя
                SelectObject(mem, app.font_main);
                SetTextColor(mem, rgb(C_TXT.0, C_TXT.1, C_TXT.2));
                let label = app.config.label_for(id_key, &it.name);
                let hovered = app.hover == i as i32;
                // START_BLOCK_ROW_WINDOW_RIGHT
                // в режиме reorder — ручка; при наведении — ✕; иначе метка
                let name_right = if app.reorder || hovered {
                    w - 28
                } else if label.is_empty() {
                    w - 10
                } else {
                    w - 96
                };
                let disp = display_name(&it.name, it.path.as_deref().and_then(|p| app.config.number_for(p)));
                dt(mem, &disp, RECT { left: 42, top, right: name_right, bottom: top + ROW }, DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS);
                if app.reorder {
                    draw_handle(mem, w - 18, top);
                } else if hovered {
                    SetTextColor(mem, rgb(C_DIM.0, C_DIM.1, C_DIM.2));
                    dt(mem, "✕", RECT { left: w - CLOSE_W, top, right: w - 6, bottom: top + ROW }, DT_SINGLELINE | DT_VCENTER | DT_CENTER);
                } else if !label.is_empty() {
                    SelectObject(mem, app.font_small);
                    SetTextColor(mem, rgb(C_DIM.0, C_DIM.1, C_DIM.2));
                    dt(mem, &label, RECT { left: w - 92, top, right: w - 10, bottom: top + ROW }, DT_SINGLELINE | DT_VCENTER | DT_RIGHT | DT_END_ELLIPSIS);
                }
                // END_BLOCK_ROW_WINDOW_RIGHT
            }
            Row::RecentHeader { app: a } => {
                if app.hover == i as i32 {
                    fill(mem, full, C_HOVER);
                }
                let def: &AppDef = &app.config.apps[*a];
                let open = app.config.is_recent_open(&def.block);
                let cnt = app.recent.iter().filter(|d| d.app == *a).count();
                SelectObject(mem, app.font_small);
                SetTextColor(mem, rgb(C_DIM.0, C_DIM.1, C_DIM.2));
                dt(mem, if open { "▾" } else { "▸" }, RECT { left: 24, top, right: 38, bottom: top + ROW }, DT_SINGLELINE | DT_VCENTER | DT_LEFT);
                dt(mem, &format!("Недавние ({})", cnt), RECT { left: 40, top, right: w - 10, bottom: top + ROW }, DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS);
            }
            Row::Recent { ridx } => {
                if app.hover == i as i32 {
                    fill(mem, full, C_HOVER);
                }
                let d: &RecentDoc = &app.recent[*ridx];
                SelectObject(mem, app.font_main);
                SetTextColor(mem, rgb(C_REC.0, C_REC.1, C_REC.2));
                dt(mem, "◌", RECT { left: 42, top, right: 56, bottom: top + ROW }, DT_SINGLELINE | DT_VCENTER | DT_LEFT);
                dt(mem, &d.name, RECT { left: 58, top, right: w - 10, bottom: top + ROW }, DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS);
            }
            Row::RecentMore { app: a } => {
                if app.hover == i as i32 {
                    fill(mem, full, C_HOVER);
                }
                let def: &AppDef = &app.config.apps[*a];
                let total = app.recent.iter().filter(|d| d.app == *a).count();
                let txt = if app.config.is_showall(&def.block) {
                    "▴ свернуть".to_string()
                } else {
                    format!("… показать все ({})", total)
                };
                SelectObject(mem, app.font_small);
                SetTextColor(mem, rgb(C_DIM.0, C_DIM.1, C_DIM.2));
                dt(mem, &txt, RECT { left: 58, top, right: w - 10, bottom: top + ROW }, DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS);
            }
            Row::SearchHeader => {
                fill(mem, full, C_SECTION);
                SelectObject(mem, app.font_small);
                SetTextColor(mem, rgb(C_DIM.0, C_DIM.1, C_DIM.2));
                dt(mem, "🔍 Найдено ещё", RECT { left: 10, top, right: w - 10, bottom: top + ROW }, DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS);
            }
            Row::SearchResult { hit } => {
                if app.hover == i as i32 {
                    fill(mem, full, C_HOVER);
                }
                let h = &app.search_hits[*hit];
                let fc = match h.color { Color::Bm25 => C_SRCH_BM25, Color::Dense => C_SRCH_DENSE };
                // иконка папки/проекта в цветной рамке (вместо тонкой полосы)
                match icon::path_icon(&h.folder) {
                    Some(hicon) => draw_framed_icon(mem, 10, top, hicon, fc),
                    None => {
                        let fy = top + (ROW - 20) / 2;
                        fill(mem, RECT { left: 10, top: fy, right: 30, bottom: fy + 20 }, fc);
                    }
                }
                SelectObject(mem, app.font_main);
                SetTextColor(mem, rgb(C_TXT.0, C_TXT.1, C_TXT.2));
                dt(mem, &folder_project(&h.folder), RECT { left: 36, top, right: w - 10, bottom: top + ROW }, DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS);
            }
        }
        // разделитель
        fill(mem, RECT { left: 0, top: top + ROW - 1, right: w, bottom: top + ROW }, C_BORDER);
        // подсветка перетаскиваемой строки
        if app.drag == Some(i as i32) {
            let pen = CreatePen(PS_SOLID, 2, rgb(C_BELL_BAR.0, C_BELL_BAR.1, C_BELL_BAR.2));
            let oldpen = SelectObject(mem, pen);
            let oldbr = SelectObject(mem, GetStockObject(NULL_BRUSH));
            let _ = Rectangle(mem, 1, top + 1, w - 1, top + ROW - 1);
            SelectObject(mem, oldpen);
            SelectObject(mem, oldbr);
            let _ = DeleteObject(pen);
        }
    }

    let pen = CreatePen(PS_SOLID, 1, rgb(C_BORDER.0, C_BORDER.1, C_BORDER.2));
    let oldpen = SelectObject(mem, pen);
    let oldbr = SelectObject(mem, GetStockObject(NULL_BRUSH));
    let _ = Rectangle(mem, 0, 0, w, h);
    SelectObject(mem, oldpen);
    SelectObject(mem, oldbr);
    let _ = DeleteObject(pen);

    let _ = BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
    SelectObject(mem, old);
    let _ = DeleteObject(bmp);
    let _ = DeleteDC(mem);
    let _ = EndPaint(hwnd, &ps);
}

// START_CONTRACT: resize
//   PURPOSE: Подогнать высоту окна под число строк и переутвердить topmost.
//   INPUTS: { hwnd: HWND; app: &mut App }
//   OUTPUTS: { () }
//   SIDE_EFFECTS: SetWindowPos
// END_CONTRACT: resize
pub unsafe fn resize(hwnd: HWND, app: &mut App) {
    let n = app.rows.len().max(1) as i32;
    let h = HEAD + ROW * n;
    if h != app.last_h {
        app.last_h = h;
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, W, h, SWP_NOMOVE | SWP_NOACTIVATE);
    } else {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

// START_CONTRACT: row_at
//   PURPOSE: Индекс строки по координате Y (или -1, если шапка/за пределами).
//   INPUTS: { y: i32; n: usize - число строк }
//   OUTPUTS: { i32 }
//   SIDE_EFFECTS: none
// END_CONTRACT: row_at
pub fn row_at(y: i32, n: usize) -> i32 {
    if y < HEAD {
        return -1;
    }
    let i = (y - HEAD) / ROW;
    if i >= 0 && (i as usize) < n {
        i
    } else {
        -1
    }
}

// START_CONTRACT: hit_test
//   PURPOSE: Определить строку и зону клика по координатам (с учётом режима reorder).
//   INPUTS: { x: i32; y: i32; rows: &[Row]; w: i32 - ширина клиента; reorder: bool - режим перетаскивания }
//   OUTPUTS: { (i32 - индекс строки или -1, Zone) }
//   SIDE_EFFECTS: none
// END_CONTRACT: hit_test
pub fn hit_test(x: i32, y: i32, rows: &[Row], w: i32, reorder: bool) -> (i32, Zone) {
    let i = row_at(y, rows.len());
    if i < 0 {
        return (-1, Zone::Body);
    }
    let row = rows[i as usize];
    if reorder {
        // в режиме reorder вся секция/окно хватается для перетаскивания (ручка — лишь подсказка)
        if matches!(row, Row::Section { .. } | Row::Window { .. }) {
            return (i, Zone::DragHandle);
        }
        return (i, Zone::Body);
    }
    let zone = match row {
        Row::Window { .. } if x >= w - CLOSE_W => Zone::Close,
        _ => Zone::Body,
    };
    (i, zone)
}

// START_CONTRACT: section_of_row
//   PURPOSE: Индекс приложения секции, к которой относится строка (ближайший Section выше).
//   INPUTS: { rows: &[Row]; i: usize - индекс строки }
//   OUTPUTS: { Option<usize> - индекс app или None }
//   SIDE_EFFECTS: none
// END_CONTRACT: section_of_row
pub fn section_of_row(rows: &[Row], i: usize) -> Option<usize> {
    let last = i.min(rows.len().saturating_sub(1));
    (0..=last).rev().find_map(|k| match rows[k] {
        Row::Section { app } => Some(app),
        _ => None,
    })
}

// START_CONTRACT: section_blocks
//   PURPOSE: Имена блоков секций в текущем видимом порядке (для drag-reorder секций).
//   INPUTS: { rows: &[Row]; apps: &[AppDef] }
//   OUTPUTS: { Vec<String> - блоки в порядке строк }
//   SIDE_EFFECTS: none
// END_CONTRACT: section_blocks
pub fn section_blocks(rows: &[Row], apps: &[AppDef]) -> Vec<String> {
    rows.iter()
        .filter_map(|r| match r {
            Row::Section { app } => apps.get(*app).map(|a| a.block.clone()),
            _ => None,
        })
        .collect()
}

// START_CONTRACT: window_names
//   PURPOSE: Имена окон секции в текущем видимом порядке (для drag-reorder окон).
//   INPUTS: { rows: &[Row]; items: &[WinItem]; app: usize - индекс приложения секции }
//   OUTPUTS: { Vec<String> - имена окон секции в порядке строк }
//   SIDE_EFFECTS: none
// END_CONTRACT: window_names
pub fn window_names(rows: &[Row], items: &[WinItem], app: usize) -> Vec<String> {
    rows.iter()
        .filter_map(|r| match r {
            Row::Window { idx } => {
                let it = &items[*idx];
                if it.app == app {
                    Some(it.name.clone())
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect()
}

// START_CONTRACT: folder_project
//   PURPOSE: Имя проекта = последний сегмент пути папки (нижний регистр).
//   INPUTS: { folder: &str }
//   OUTPUTS: { String }
//   SIDE_EFFECTS: none
// END_CONTRACT: folder_project
pub fn folder_project(folder: &str) -> String {
    folder.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or(folder).to_lowercase()
}

// START_CONTRACT: display_name
//   PURPOSE: Имя строки окна с нумерацией дублей по-Windows: «name» или «name (N)» при N>=2 — Phase-15.
//   INPUTS: { name: &str; number: Option<i32> - стабильный № из M-CONFIG (None = нет пути/не присвоен) }
//   OUTPUTS: { String }
//   SIDE_EFFECTS: none
// END_CONTRACT: display_name
pub fn display_name(name: &str, number: Option<i32>) -> String {
    match number {
        Some(n) if n >= 2 => format!("{} ({})", name, n),
        _ => name.to_string(), // первый дубль (N=1) и одиночные — без числа
    }
}

// START_CONTRACT: search_color_for
//   PURPOSE: Цвет подсветки для проекта, если он среди совпадений поиска.
//   INPUTS: { hits: &[FolderHit]; project_lower: &str }
//   OUTPUTS: { Option<Color> }
//   SIDE_EFFECTS: none
// END_CONTRACT: search_color_for
pub fn search_color_for(hits: &[FolderHit], project_lower: &str) -> Option<Color> {
    hits.iter().find(|h| folder_project(&h.folder) == project_lower).map(|h| h.color)
}

// START_CONTRACT: search_result_rows
//   PURPOSE: Строки блока «Найдено ещё» — совпадения поиска, чей проект НЕ открыт.
//   INPUTS: { hits: &[FolderHit]; open_projects: &HashSet<String> - открытые проекты (lower) }
//   OUTPUTS: { Vec<Row> - SearchHeader + SearchResult по закрытым; пусто, если закрытых нет }
//   SIDE_EFFECTS: none
// END_CONTRACT: search_result_rows
pub fn search_result_rows(hits: &[FolderHit], open_projects: &HashSet<String>) -> Vec<Row> {
    let closed: Vec<usize> = hits
        .iter()
        .enumerate()
        .filter(|(_, h)| !open_projects.contains(&folder_project(&h.folder)))
        .map(|(i, _)| i)
        .collect();
    if closed.is_empty() {
        return Vec::new();
    }
    let mut rows = vec![Row::SearchHeader];
    rows.extend(closed.into_iter().map(|hit| Row::SearchResult { hit }));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_apps;
    use crate::win_enum::WinItem;

    fn item(app: usize, name: &str) -> WinItem {
        WinItem { hwnd: HWND(std::ptr::null_mut()), app, name: name.to_string(), path: None, ordinal: 0 }
    }

    fn mock_cfg() -> Config {
        Config {
            apps: default_apps(),
            projects: Default::default(),
            proj_numbers: Default::default(),
            collapsed: Default::default(),
            recent_expanded: Default::default(),
            recent_showall: Default::default(),
            section_order: Default::default(),
            window_order: Default::default(),
            font_face: crate::config::DEFAULT_FONT.to_string(),
            font_size: crate::config::DEFAULT_FONT_SIZE,
            font_weight: crate::config::DEFAULT_FONT_WEIGHT,
            pos: None,
            search_db: String::new(),
            search_cmd: String::new(),
            search_port: crate::config::DEFAULT_SEARCH_PORT,
            chats_db: String::new(),
            files_db: String::new(),
            projects_root: String::new(),
            search_files: false,
            sort_recent: false,
            cfg_path: std::path::PathBuf::new(),
        }
    }

    #[test]
    fn build_rows_order_modes() {
        let apps = default_apps();
        let mut bravo = item(0, "Bravo");
        bravo.ordinal = 1; // открыт раньше
        let mut alpha = item(0, "Alpha");
        alpha.ordinal = 2; // открыт позже
        let items = vec![bravo, alpha];
        let names = |cfg: &Config| -> Vec<String> {
            build_rows(&items, &[], &apps, cfg)
                .into_iter()
                .filter_map(|r| match r {
                    Row::Window { idx } => Some(items[idx].name.clone()),
                    _ => None,
                })
                .collect()
        };
        let mut c = mock_cfg();
        c.set_sort_recent(false); // alpha: по имени
        assert_eq!(names(&c), vec!["Alpha", "Bravo"]);
        c.set_sort_recent(true); // recent: по ordinal (позже -> ниже)
        assert_eq!(names(&c), vec!["Bravo", "Alpha"]);
    }

    #[test]
    fn display_name_numbers_duplicates() {
        assert_eq!(display_name("claudebar", None), "claudebar"); // нет пути/номера
        assert_eq!(display_name("claudebar", Some(1)), "claudebar"); // первый дубль — без числа
        assert_eq!(display_name("claudebar", Some(2)), "claudebar (2)");
        assert_eq!(display_name("claudebar", Some(3)), "claudebar (3)");
    }

    #[test]
    fn row_at_maps_rows() {
        assert_eq!(row_at(0, 5), -1);
        assert_eq!(row_at(HEAD, 3), 0);
        assert_eq!(row_at(HEAD + ROW, 3), 1);
        assert_eq!(row_at(HEAD + 3 * ROW, 3), -1);
    }

    #[test]
    fn build_rows_groups_with_headers() {
        let apps = default_apps();
        let cfg = Config {
            apps: default_apps(),
            projects: Default::default(),
            proj_numbers: Default::default(),
            collapsed: Default::default(),
            recent_expanded: Default::default(),
            recent_showall: Default::default(),
            section_order: Default::default(),
            window_order: Default::default(),
            font_face: crate::config::DEFAULT_FONT.to_string(),
            font_size: crate::config::DEFAULT_FONT_SIZE,
            font_weight: crate::config::DEFAULT_FONT_WEIGHT,
            pos: None,
            search_db: String::new(),
            search_cmd: String::new(),
            search_port: crate::config::DEFAULT_SEARCH_PORT,
            chats_db: String::new(),
            files_db: String::new(),
            projects_root: String::new(),
            search_files: false,
            sort_recent: false,
            cfg_path: std::path::PathBuf::new(),
        };
        // 2 окна VS Code (app 0) + 1 окно Word (app 2)
        let items = vec![item(0, "A"), item(0, "B"), item(2, "Doc")];
        let rows = build_rows(&items, &[], &apps, &cfg);
        // section VS Code + 2 окна + section Word + 1 окно = 5 строк
        assert_eq!(rows.len(), 5);
        assert!(matches!(rows[0], Row::Section { app: 0 }));
        assert!(matches!(rows[1], Row::Window { .. }));
        assert!(matches!(rows[3], Row::Section { app: 2 }));
    }

    #[test]
    fn hit_test_window_close_vs_body() {
        let rows = vec![Row::Window { idx: 0 }];
        // тело строки окна (не reorder)
        assert_eq!(hit_test(50, HEAD + 2, &rows, W, false), (0, Zone::Body));
        // правая зона ✕
        assert_eq!(hit_test(W - 5, HEAD + 2, &rows, W, false), (0, Zone::Close));
        // секция: даже справа — Body (✕ только у окон)
        let sec = vec![Row::Section { app: 0 }];
        assert_eq!(hit_test(W - 5, HEAD + 2, &sec, W, false), (0, Zone::Body));
        // за пределами строк
        assert_eq!(hit_test(10, 0, &rows, W, false).0, -1);
    }

    #[test]
    fn hit_test_reorder_gives_drag_handle() {
        let rows = vec![Row::Window { idx: 0 }, Row::Section { app: 0 }, Row::RecentHeader { app: 0 }];
        // regression: в режиме reorder хватается ВСЯ строка секции/окна (а не только правые 24px)
        assert_eq!(hit_test(50, HEAD + 2, &rows, W, true), (0, Zone::DragHandle));
        assert_eq!(hit_test(W - 5, HEAD + 2, &rows, W, true), (0, Zone::DragHandle));
        assert_eq!(hit_test(50, HEAD + ROW + 2, &rows, W, true), (1, Zone::DragHandle));
        // не-draggable строки (заголовок недавних) — Body даже в reorder
        assert_eq!(hit_test(50, HEAD + 2 * ROW + 2, &rows, W, true), (2, Zone::Body));
    }

    #[test]
    fn section_of_row_and_blocks() {
        let apps = default_apps();
        // Section(Word=2), Window, Window
        let rows = vec![Row::Section { app: 2 }, Row::Window { idx: 0 }, Row::Window { idx: 1 }];
        assert_eq!(section_of_row(&rows, 0), Some(2));
        assert_eq!(section_of_row(&rows, 2), Some(2)); // окно относится к Word
        assert_eq!(section_blocks(&rows, &apps), vec!["Word".to_string()]);
    }

    #[test]
    fn build_rows_hides_collapsed_section_body() {
        let apps = default_apps();
        let mut cfg = Config {
            apps: default_apps(),
            projects: Default::default(),
            proj_numbers: Default::default(),
            collapsed: Default::default(),
            recent_expanded: Default::default(),
            recent_showall: Default::default(),
            section_order: Default::default(),
            window_order: Default::default(),
            font_face: crate::config::DEFAULT_FONT.to_string(),
            font_size: crate::config::DEFAULT_FONT_SIZE,
            font_weight: crate::config::DEFAULT_FONT_WEIGHT,
            pos: None,
            search_db: String::new(),
            search_cmd: String::new(),
            search_port: crate::config::DEFAULT_SEARCH_PORT,
            chats_db: String::new(),
            files_db: String::new(),
            projects_root: String::new(),
            search_files: false,
            sort_recent: false,
            cfg_path: std::path::PathBuf::new(),
        };
        cfg.toggle_collapsed("VS Code");
        let items = vec![item(0, "A"), item(0, "B")];
        let rows = build_rows(&items, &[], &apps, &cfg);
        // только заголовок, тело скрыто
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0], Row::Section { app: 0 }));
    }

    #[test]
    fn folder_project_basename_lower() {
        assert_eq!(folder_project("D:\\Python\\hh_answer"), "hh_answer");
        assert_eq!(folder_project("D:/Python/ClaudeBar/"), "claudebar");
    }

    #[test]
    fn search_color_and_result_rows() {
        let hits = vec![
            FolderHit { folder: "D:\\Python\\hh_answer".into(), color: Color::Bm25, score: 4.0 },
            FolderHit { folder: "D:\\Python\\margo".into(), color: Color::Dense, score: 0.7 },
        ];
        // открытый проект hh_answer -> его цвет; неизвестный -> None
        assert_eq!(search_color_for(&hits, "hh_answer"), Some(Color::Bm25));
        assert_eq!(search_color_for(&hits, "unknown"), None);
        // «Найдено ещё»: открыт только hh_answer -> margo в закрытых
        let mut open = HashSet::new();
        open.insert("hh_answer".to_string());
        let rows = search_result_rows(&hits, &open);
        assert_eq!(rows.len(), 2); // header + margo
        assert!(matches!(rows[0], Row::SearchHeader));
        assert!(matches!(rows[1], Row::SearchResult { hit: 1 }));
    }
}
