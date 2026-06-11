// FILE: src/render.rs
// VERSION: 1.4.0
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
//   build_rows        - сгруппировать окна в строки секций с учётом свёрнутости
//   paint             - отрисовать строки (заголовки секций + окна + ✕ на hover)
//   resize            - подогнать высоту окна под число строк
//   row_at            - индекс строки по координате Y
//   hit_test          - (строка, Zone) по координатам клика
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.6.0 - Phase-7 Step 3: Row::RecentMore — «показать все» недавних сверх 6 (VISIBLE_RECENT).
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
use crate::win_enum::WinItem;
use crate::App;

// геометрия
pub const W: i32 = 252;
pub const HEAD: i32 = 24;
pub const ROW: i32 = 30;
const SWATCH: i32 = 14;
const CLOSE_W: i32 = 24; // ширина правой зоны кнопки ✕ на строке окна
const VISIBLE_RECENT: usize = 6; // сколько недавних показывать до «показать все»

// Строка панели: заголовок секции приложения или окно внутри секции.
#[derive(Clone, Copy)]
pub enum Row {
    Section { app: usize },
    Window { idx: usize }, // индекс в App.items
    RecentHeader { app: usize }, // под-заголовок «Недавние»
    Recent { ridx: usize }, // индекс в App.recent
    RecentMore { app: usize }, // строка «показать все / свернуть» недавних
}

// Зона клика внутри строки.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Zone {
    Body,  // основное тело строки (активация / открытие / сворачивание)
    Close, // правая зона ✕ на строке окна
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
    let mut rec_by_app: Vec<Vec<usize>> = vec![Vec::new(); apps.len()];
    for (i, d) in recent.iter().enumerate() {
        if d.app < apps.len() {
            rec_by_app[d.app].push(i);
        }
    }
    let mut rows = Vec::new();
    // START_BLOCK_GROUP_SECTIONS
    for a in 0..apps.len() {
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
    fill(mem, RECT { left: 0, top: 0, right: w, bottom: HEAD }, C_HEAD);
    SetBkMode(mem, TRANSPARENT);

    // шапка
    SelectObject(mem, app.font_small);
    SetTextColor(mem, rgb(C_DIM.0, C_DIM.1, C_DIM.2));
    dt(mem, "≡ ClaudeBar", RECT { left: 10, top: 0, right: w - 24, bottom: HEAD }, DT_SINGLELINE | DT_VCENTER | DT_LEFT);
    dt(mem, "✕", RECT { left: w - 24, top: 0, right: w, bottom: HEAD }, DT_SINGLELINE | DT_VCENTER | DT_CENTER);

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
                let cnt = app.items.iter().filter(|it| it.app == *a).count();
                SelectObject(mem, app.font_small);
                SetTextColor(mem, rgb(C_DIM.0, C_DIM.1, C_DIM.2));
                dt(mem, &cnt.to_string(), RECT { left: w - 34, top, right: w - 12, bottom: top + ROW }, DT_SINGLELINE | DT_VCENTER | DT_RIGHT);
            }
            Row::Window { idx } => {
                let it: &WinItem = &app.items[*idx];
                // START_BLOCK_ROW_BG_WINDOW
                let belling = app.bell.contains(&it.name.to_lowercase());
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
                // цветная плашка (с отступом — окна вложены в секцию)
                let cy = top + (ROW - SWATCH) / 2;
                let (_, r, g, b) = PALETTE[app.config.color_idx(&it.name)];
                fill(mem, RECT { left: 20, top: cy, right: 20 + SWATCH, bottom: cy + SWATCH }, (r, g, b));
                // имя
                SelectObject(mem, app.font_main);
                SetTextColor(mem, rgb(C_TXT.0, C_TXT.1, C_TXT.2));
                let label = app.config.label(&it.name);
                let hovered = app.hover == i as i32;
                // START_BLOCK_ROW_WINDOW_RIGHT
                // при наведении правый край отдаём под ✕, метку прячем
                let name_right = if hovered {
                    w - 28
                } else if label.is_empty() {
                    w - 10
                } else {
                    w - 96
                };
                dt(mem, &it.name, RECT { left: 42, top, right: name_right, bottom: top + ROW }, DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS);
                if hovered {
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
        }
        // разделитель
        fill(mem, RECT { left: 0, top: top + ROW - 1, right: w, bottom: top + ROW }, C_BORDER);
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
//   PURPOSE: Определить строку и зону клика по координатам.
//   INPUTS: { x: i32; y: i32; rows: &[Row]; w: i32 - ширина клиента }
//   OUTPUTS: { (i32 - индекс строки или -1, Zone) }
//   SIDE_EFFECTS: none
// END_CONTRACT: hit_test
pub fn hit_test(x: i32, y: i32, rows: &[Row], w: i32) -> (i32, Zone) {
    let i = row_at(y, rows.len());
    if i < 0 {
        return (-1, Zone::Body);
    }
    let zone = match rows[i as usize] {
        Row::Window { .. } if x >= w - CLOSE_W => Zone::Close,
        _ => Zone::Body,
    };
    (i, zone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_apps;
    use crate::win_enum::WinItem;

    fn item(app: usize, name: &str) -> WinItem {
        WinItem { hwnd: HWND(std::ptr::null_mut()), app, name: name.to_string() }
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
            collapsed: Default::default(),
            recent_expanded: Default::default(),
            recent_showall: Default::default(),
            pos: None,
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
        // тело строки окна
        assert_eq!(hit_test(50, HEAD + 2, &rows, W), (0, Zone::Body));
        // правая зона ✕
        assert_eq!(hit_test(W - 5, HEAD + 2, &rows, W), (0, Zone::Close));
        // секция: даже справа — Body (✕ только у окон)
        let sec = vec![Row::Section { app: 0 }];
        assert_eq!(hit_test(W - 5, HEAD + 2, &sec, W), (0, Zone::Body));
        // за пределами строк
        assert_eq!(hit_test(10, 0, &rows, W).0, -1);
    }

    #[test]
    fn build_rows_hides_collapsed_section_body() {
        let apps = default_apps();
        let mut cfg = Config {
            apps: default_apps(),
            projects: Default::default(),
            collapsed: Default::default(),
            recent_expanded: Default::default(),
            recent_showall: Default::default(),
            pos: None,
            cfg_path: std::path::PathBuf::new(),
        };
        cfg.toggle_collapsed("VS Code");
        let items = vec![item(0, "A"), item(0, "B")];
        let rows = build_rows(&items, &[], &apps, &cfg);
        // только заголовок, тело скрыто
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0], Row::Section { app: 0 }));
    }
}
