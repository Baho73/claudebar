// FILE: src/tooltip.rs
// VERSION: 1.1.0
// START_MODULE_CONTRACT
//   PURPOSE: Собственный popup-тултип (Phase-13 Ф-B): окно подсказки, dwell-таймер, позиционирование по монитору, текст цели.
//   SCOPE: create_tooltip/arm_tip/show_tooltip/hide_tooltip (плюмбинг окна + dwell) и tip_text_for (правила/путь/сниппет) поверх App.
//   DEPENDS: M-STATE, M-RENDER, M-SEARCH, M-RECENT
//   LINKS: M-TOOLTIP
//   ROLE: UI_COMPONENT
//   MAP_MODE: EXPORTS
//   NOTE: Вынесено из main.rs (рефактор god-object, FPF D-5). Константы dwell (ID_TIP_TIMER/TIP_DELAY/TIP_SEARCHBOX) и хелперы edit_text/recent_path остаются в M-MAIN (их делит wndproc), импортируются сюда.
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   create_tooltip - создать popup-окно подсказки (WNDCLASS ClbarTip) один раз при старте; сохранить HWND в App.tooltip
//   tip_proc       - WM_PAINT попапа: фон + рамка + текст TIP_SHOW панельным шрифтом
//   arm_tip        - сменить цель подсказки (App.tip_row): спрятать текущую, перезавести dwell-таймер
//   show_tooltip   - по срабатыванию dwell: измерить текст, привязать к монитору под курсором, показать
//   hide_tooltip   - спрятать попап
//   tip_text_for   - текст цели: правила поиска / полный путь-имя окна / путь+сниппет результата
//   tip_basename   - basename папки (lower) для сопоставления окна с хитом поиска
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.1.0 - M-MAIL: к подсказке строки дописывается «✉ 7 новых: Mail.ru 6, Яндекс 1» (источники и количества) для окон и «Недавних».
//   v1.0.0 - рефактор: tooltip-кластер вынесен из main.rs в отдельный UI-модуль (M-TOOLTIP), без изменения поведения.
// END_CHANGE_SUMMARY

use std::cell::RefCell;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::{edit_text, recent_path, render, search, App, APP, ID_TIP_TIMER, TIP_DELAY, TIP_SEARCHBOX};

const C_TIP_BG: u32 = 0x00E1FFFF; // фон подсказки (инфо-жёлтый, RGB 255,255,225)
const C_TIP_TXT: u32 = 0x00202020; // тёмный текст подсказки
const SEARCH_RULES: &str = "Правила поиска:\r\n  пробел — И (оба слова)\r\n  a+b — точная фраза (подряд)\r\n  a++b — рядом (NEAR)\r\n  -слово — исключить\r\n  OR — или (заглавными)\r\nIP / путь / дата ищутся как фраза.";

thread_local! {
    // текст текущей подсказки (для собственной отрисовки попапа)
    static TIP_SHOW: RefCell<String> = const { RefCell::new(String::new()) };
}

// Создать собственное popup-окно подсказки (один раз при старте).
pub(crate) unsafe fn create_tooltip(hwnd: HWND) {
    let hinst = APP.with(|c| c.borrow().as_ref().map(|a| a.hinst).unwrap_or_default());
    let cls = WNDCLASSW {
        lpfnWndProc: Some(tip_proc),
        hInstance: hinst,
        lpszClassName: w!("ClbarTip"),
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassW(&cls); // повторная регистрация вернёт 0 — не страшно
    let tip = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        w!("ClbarTip"),
        PCWSTR::null(),
        WS_POPUP,
        0,
        0,
        0,
        0,
        hwnd,
        None,
        hinst,
        None,
    )
    .unwrap_or_default();
    APP.with(|c| {
        if let Some(a) = c.borrow_mut().as_mut() {
            a.tooltip = tip;
        }
    });
}

// Отрисовка попапа: фон + рамка + текст панельным шрифтом.
extern "system" fn tip_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_PAINT {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let bg = CreateSolidBrush(COLORREF(C_TIP_BG));
            FillRect(hdc, &rc, bg);
            let _ = DeleteObject(bg);
            let frame = CreateSolidBrush(COLORREF(0x0080_8080));
            FrameRect(hdc, &rc, frame);
            let _ = DeleteObject(frame);
            let font = APP.with(|c| c.borrow().as_ref().map(|a| a.font_small).unwrap_or_default());
            let old = SelectObject(hdc, font);
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(C_TIP_TXT));
            let mut tr = RECT { left: 7, top: 4, right: rc.right - 4, bottom: rc.bottom - 2 };
            let mut wide: Vec<u16> = TIP_SHOW.with(|t| t.borrow().encode_utf16().collect());
            DrawTextW(hdc, &mut wide, &mut tr, DT_LEFT | DT_TOP | DT_NOPREFIX | DT_WORDBREAK);
            SelectObject(hdc, old);
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

// Сменить цель подсказки: спрятать текущую, перезавести dwell-таймер.
pub(crate) unsafe fn arm_tip(hwnd: HWND, row: i32) {
    let changed = APP.with(|c| {
        c.borrow_mut().as_mut().is_some_and(|a| {
            let ch = a.tip_row != row;
            a.tip_row = row;
            ch
        })
    });
    if changed {
        hide_tooltip(hwnd);
        let _ = KillTimer(hwnd, ID_TIP_TIMER);
        if row != -1 {
            let _ = SetTimer(hwnd, ID_TIP_TIMER, TIP_DELAY, None);
        }
    }
}

// Показать подсказку у курсора (по срабатыванию dwell-таймера).
pub(crate) unsafe fn show_tooltip(_hwnd: HWND) {
    let (tip, font) =
        APP.with(|c| c.borrow().as_ref().map(|a| (a.tooltip, a.font_small)).unwrap_or_default());
    if tip.0.is_null() {
        return;
    }
    let text = APP.with(|c| {
        let b = c.borrow();
        b.as_ref().and_then(|a| tip_text_for(a, a.tip_row))
    });
    let Some(text) = text else {
        return;
    };
    TIP_SHOW.with(|t| *t.borrow_mut() = text.clone());
    // измерить текст панельным шрифтом, с переносом длинных строк по словам
    const TIP_MAXW: i32 = 480;
    let hdc = GetDC(tip);
    let old = SelectObject(hdc, font);
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let mut rc = RECT { left: 0, top: 0, right: TIP_MAXW, bottom: 0 };
    DrawTextW(hdc, &mut wide, &mut rc, DT_CALCRECT | DT_LEFT | DT_TOP | DT_NOPREFIX | DT_WORDBREAK);
    SelectObject(hdc, old);
    ReleaseDC(tip, hdc);
    let w = (rc.right - rc.left + 14).clamp(40, TIP_MAXW + 14);
    let h = (rc.bottom - rc.top + 8).clamp(20, 400);
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    // рабочая область монитора под курсором -> привязка к границам экрана
    let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let _ = GetMonitorInfoW(mon, &mut mi);
    let wa = mi.rcWork;
    // по умолчанию справа от курсора; у правого края — влево от курсора
    let flip_left = pt.x + 18 + w > wa.right;
    let mut x = if flip_left { pt.x - 18 - w } else { pt.x + 18 };
    x = x.clamp(wa.left, (wa.right - w).max(wa.left));
    // верх у курсора; если ушли влево (под мышь) или вниз не лезет — выше курсора
    let mut y = if flip_left { pt.y - h - 14 } else { pt.y - 10 };
    if y + h > wa.bottom {
        y = pt.y - h - 14;
    }
    y = y.clamp(wa.top, (wa.bottom - h).max(wa.top));
    let _ = SetWindowPos(tip, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
    let _ = ShowWindow(tip, SW_SHOWNOACTIVATE);
    let _ = InvalidateRect(tip, None, BOOL(1));
}

pub(crate) unsafe fn hide_tooltip(_hwnd: HWND) {
    let tip = APP.with(|c| c.borrow().as_ref().map(|a| a.tooltip).unwrap_or_default());
    if !tip.0.is_null() {
        let _ = ShowWindow(tip, SW_HIDE);
    }
}

// Текст подсказки для цели: правила / полный путь-имя / путь+сниппет.
fn tip_text_for(app: &App, tip_row: i32) -> Option<String> {
    if tip_row == TIP_SEARCHBOX {
        return Some(SEARCH_RULES.to_string());
    }
    if tip_row < 0 {
        return None;
    }
    // Входящие (M-MAIL): строка «7 новых: Mail.ru 6, Яндекс 1» дописывается к обычной подсказке,
    // чтобы наведение на значок объясняло, что за источники и сколько.
    let mail_line = |path: Option<&str>, name: &str| {
        let ms = crate::mail::mails_for_row(&app.mails, path, name);
        crate::mail::merge_mails(&ms).map(|m| crate::mail::tooltip_text(&m))
    };
    let with_mail = |base: String, extra: Option<String>| match extra {
        Some(m) => format!("{base}\r\n\u{2709} {m}"),
        None => base,
    };
    match app.rows.get(tip_row as usize)? {
        render::Row::Window { idx } => app.items.get(*idx).map(|it| {
            // совпавшее с поиском открытое окно -> полный путь папки из хита, иначе имя
            let proj = it.name.to_lowercase();
            let base = app
                .search_hits
                .iter()
                .find(|h| tip_basename(&h.folder) == proj)
                .map(|h| h.folder.clone())
                .unwrap_or_else(|| it.name.clone());
            with_mail(base, mail_line(it.path.as_deref(), &it.name))
        }),
        render::Row::Recent { ridx } => app.recent.get(*ridx).map(|d| {
            let dpath = match &d.open {
                crate::recent::OpenCmd::Editor { folder, .. } => Some(folder.as_str()),
                crate::recent::OpenCmd::Lnk(_) => None,
            };
            with_mail(recent_path(d), mail_line(dpath, &d.name))
        }),
        render::Row::SearchResult { hit } => app.search_hits.get(*hit).map(|h| {
            let q = edit_text(app.search_edit);
            // сниппет: сперва из чатов, иначе (если scope «+Файлы») из файлов
            let snip = search::snippet_for(&app.config.chats_db, &q, &h.folder).or_else(|| {
                app.config
                    .search_files
                    .then(|| search::snippet_for(&app.config.files_db, &q, &h.folder))
                    .flatten()
            });
            match snip {
                Some(s) => format!("{}\r\n{}", h.folder, s),
                None => h.folder.clone(),
            }
        }),
        _ => None,
    }
}

// basename папки в нижнем регистре (для сопоставления окна с хитом поиска).
fn tip_basename(p: &str) -> String {
    p.rsplit(|c| c == '\\' || c == '/').next().unwrap_or(p).to_lowercase()
}
