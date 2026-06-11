// FILE: src/render.rs
// VERSION: 1.0.0
// START_MODULE_CONTRACT
//   PURPOSE: Отрисовка панели (GDI, двойной буфер), геометрия строк и hit-test по координате.
//   SCOPE: константы геометрии/цветов, dt/fill-помощники, paint, resize окна, row_at.
//   DEPENDS: M-CONFIG (палитра, цвета/метки проектов)
//   LINKS: M-RENDER
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   W, HEAD, ROW   - геометрия панели (ширина, высота шапки, высота строки)
//   paint          - отрисовать панель из состояния App
//   resize         - подогнать высоту окна под число строк, переутвердить topmost
//   row_at         - индекс строки по координате Y (или -1)
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.0.0 - Выделено из монолита main.rs (Phase-1, Step 4). Паритет v0.1.
// END_CHANGE_SUMMARY

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::PALETTE;
use crate::App;

// геометрия
pub const W: i32 = 252;
pub const HEAD: i32 = 24;
pub const ROW: i32 = 30;
const SWATCH: i32 = 14;

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}
const C_BG: (u8, u8, u8) = (16, 21, 38);
const C_HEAD: (u8, u8, u8) = (26, 34, 58);
const C_TXT: (u8, u8, u8) = (223, 230, 243);
const C_DIM: (u8, u8, u8) = (150, 165, 200);
const C_ACTIVE: (u8, u8, u8) = (34, 52, 96);
const C_HOVER: (u8, u8, u8) = (24, 32, 60);
const C_BORDER: (u8, u8, u8) = (40, 54, 90);

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

// START_CONTRACT: paint
//   PURPOSE: Отрисовать панель из текущего состояния App в окно hwnd.
//   INPUTS: { hwnd: HWND - окно панели; app: &App - состояние }
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

    // фон + рамка
    fill(mem, RECT { left: 0, top: 0, right: w, bottom: h }, C_BG);
    fill(mem, RECT { left: 0, top: 0, right: w, bottom: HEAD }, C_HEAD);
    SetBkMode(mem, TRANSPARENT);

    // шапка
    SelectObject(mem, app.font_small);
    SetTextColor(mem, rgb(C_DIM.0, C_DIM.1, C_DIM.2));
    dt(
        mem,
        "≡ ClaudeBar",
        RECT { left: 10, top: 0, right: w - 24, bottom: HEAD },
        DT_SINGLELINE | DT_VCENTER | DT_LEFT,
    );
    dt(
        mem,
        "✕",
        RECT { left: w - 24, top: 0, right: w, bottom: HEAD },
        DT_SINGLELINE | DT_VCENTER | DT_CENTER,
    );

    if app.items.is_empty() {
        SetTextColor(mem, rgb(C_DIM.0, C_DIM.1, C_DIM.2));
        dt(
            mem,
            "нет окон Claude Code",
            RECT { left: 10, top: HEAD, right: w - 10, bottom: HEAD + ROW },
            DT_SINGLELINE | DT_VCENTER | DT_LEFT,
        );
    }

    let fg = GetForegroundWindow();
    for (i, it) in app.items.iter().enumerate() {
        let top = HEAD + i as i32 * ROW;
        let row = RECT { left: 0, top, right: w, bottom: top + ROW };
        if it.hwnd == fg {
            fill(mem, row, C_ACTIVE);
        } else if app.hover == i as i32 {
            fill(mem, row, C_HOVER);
        }
        // цветная плашка
        let cy = top + (ROW - SWATCH) / 2;
        let (_, r, g, b) = PALETTE[app.config.color_idx(&it.name)];
        fill(
            mem,
            RECT { left: 10, top: cy, right: 10 + SWATCH, bottom: cy + SWATCH },
            (r, g, b),
        );
        // имя проекта
        SelectObject(mem, app.font_main);
        SetTextColor(mem, rgb(C_TXT.0, C_TXT.1, C_TXT.2));
        let label = app.config.label(&it.name);
        let right_pad = if label.is_empty() { 10 } else { 96 };
        dt(
            mem,
            &it.name,
            RECT { left: 32, top, right: w - right_pad, bottom: top + ROW },
            DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS,
        );
        // метка справа
        if !label.is_empty() {
            SelectObject(mem, app.font_small);
            SetTextColor(mem, rgb(C_DIM.0, C_DIM.1, C_DIM.2));
            dt(
                mem,
                &label,
                RECT { left: w - 92, top, right: w - 10, bottom: top + ROW },
                DT_SINGLELINE | DT_VCENTER | DT_RIGHT | DT_END_ELLIPSIS,
            );
        }
        // разделитель
        fill(
            mem,
            RECT { left: 0, top: top + ROW - 1, right: w, bottom: top + ROW },
            C_BORDER,
        );
    }
    // внешняя рамка
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
//   INPUTS: { hwnd: HWND; app: &mut App - кэш высоты last_h }
//   OUTPUTS: { () }
//   SIDE_EFFECTS: SetWindowPos
// END_CONTRACT: resize
pub unsafe fn resize(hwnd: HWND, app: &mut App) {
    let n = app.items.len().max(1) as i32;
    let h = HEAD + ROW * n;
    if h != app.last_h {
        app.last_h = h;
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, W, h, SWP_NOMOVE | SWP_NOACTIVATE);
    } else {
        // переутвердить topmost без изменения размера/позиции
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

// START_CONTRACT: row_at
//   PURPOSE: Индекс строки по координате Y (или -1, если шапка/за пределами).
//   INPUTS: { y: i32; n: usize - число строк }
//   OUTPUTS: { i32 - индекс строки или -1 }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_at_header_is_minus_one() {
        assert_eq!(row_at(0, 5), -1);
        assert_eq!(row_at(HEAD - 1, 5), -1);
    }

    #[test]
    fn row_at_maps_rows() {
        assert_eq!(row_at(HEAD, 3), 0);
        assert_eq!(row_at(HEAD + ROW, 3), 1);
        assert_eq!(row_at(HEAD + 2 * ROW + 5, 3), 2);
    }

    #[test]
    fn row_at_beyond_last_is_minus_one() {
        assert_eq!(row_at(HEAD + 3 * ROW, 3), -1);
    }
}
