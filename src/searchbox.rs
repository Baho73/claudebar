// FILE: src/searchbox.rs
// VERSION: 1.0.0
// START_MODULE_CONTRACT
//   PURPOSE: Виджет поиска в шапке: EDIT-поле (субкласс Enter/Esc/✕/▾), выпадающая история запросов, цвет поля, cue-подсказка.
//   SCOPE: создание EDIT+LISTBOX, субкласс-процедура, значок ✕/▾, история (файл %APPDATA%\claudebar\search_history.txt), диспетчеризация WM_COMMAND/WM_CTLCOLOREDIT от родителя.
//   DEPENDS: M-STATE, M-RENDER, M-TOOLTIP, M-MAIN (run_live_search/commit_search_enter — оркестрация поиска остаётся в M-MAIN)
//   EFFECTS: своё: %APPDATA%\claudebar\search_history.txt (история запросов)
//   REVERT:  удалить search_history.txt
//   LINKS: M-SEARCHBOX
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
//   NOTE: Вынесено из main.rs (рефактор god-object, FPF D-5). Сам поиск (M-SEARCH/M-INDEX, потоки, WM_APP_SEARCH) не здесь — здесь только окно ввода и история.
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   create_search_box     - создать EDIT-поле в шапке + скрытый LISTBOX истории (один раз при старте)
//   on_command            - обработать WM_COMMAND от родителя: EN_CHANGE поля -> живой поиск; LBN_SELCHANGE истории -> подстановка. true = обработано
//   ctl_color_edit        - WM_CTLCOLOREDIT: фон/текст поля + кэш кисти; возвращает HBRUSH как isize
//   set_search_cue        - сменить cue-подсказку пустого поля (лёгкий индикатор индексации)
//   clear_search          - Esc/✕: записать запрос в историю и очистить поле
//   search_edit_proc      - субкласс EDIT: Enter/Esc/стрелки по истории, ✕/▾, hover-тултип, дорисовка значка
//   draw_field_icon       - значок в правом отступе поля: ✕ (есть текст) или ▾ (есть история)
//   record_history / load_history / save_history - история запросов (дедуп, свежие первыми, лимит HIST_MAX)
//   show_history_dropdown / hide_history_dropdown / pick_history - выпадающий список истории
//   SEARCH_MIN            - минимум символов для живого BM25 (делится с run_live_search в M-MAIN)
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.0.0 - рефактор: кластер поля поиска и истории вынесен из main.rs в отдельный UI-модуль (M-SEARCHBOX), без изменения поведения.
// END_CHANGE_SUMMARY

use std::cell::Cell;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::tooltip::arm_tip;
use crate::{commit_search_enter, edit_text, render, run_live_search, APP, TIP_SEARCHBOX, WM_MOUSELEAVE};

pub(crate) const SEARCH_MIN: usize = 3; // живой BM25 начинается с N символов
const ID_SEARCH: usize = 40; // EDIT-поле поиска в шапке (WM_COMMAND EN_CHANGE)
const ID_HIST_LIST: usize = 41; // id child-LISTBOX выпадающей истории
const EM_SETCUEBANNER: u32 = 0x1501; // подсказка-заглушка в пустом EDIT
const EM_SETMARGINS: u32 = 0x00D3;
const EC_RIGHTMARGIN: u32 = 0x0002;
const CLEAR_W: i32 = 18; // зона значка справа в поле поиска (✕ очистка / ▾ история)
const HIST_MAX: usize = 15; // лимит истории поисков
// сообщения/стиль LISTBOX
const LBS_NOTIFY: u32 = 0x0001;
const LB_ADDSTRING: u32 = 0x0180;
const LB_RESETCONTENT: u32 = 0x0184;
const LB_SETCURSEL: u32 = 0x0186;
const LB_GETCURSEL: u32 = 0x0188;
const LB_GETTEXT: u32 = 0x0189;
const LB_GETTEXTLEN: u32 = 0x018A;
const LB_GETCOUNT: u32 = 0x018B;
const LB_GETITEMHEIGHT: u32 = 0x01A1;
const LBN_SELCHANGE: u32 = 1;
const C_SEARCH_BG: u32 = 0x00ECC86D; // фон поля поиска = палитра «Голубой» (RGB 109,200,236), как квадратик voice-smeta
const C_SEARCH_TXT: u32 = 0x003C2319; // тёмный текст поля (RGB 25,35,60)

thread_local! {
    // старый WNDPROC EDIT-поля поиска (для субкласса Enter/Esc)
    static SEARCH_OLDPROC: Cell<isize> = const { Cell::new(0) };
}

thread_local! {
    // кисть фона поля поиска (создаётся один раз)
    static SEARCH_BRUSH: Cell<isize> = const { Cell::new(0) };
}

// Создать постоянное окошко поиска в шапке (один раз при старте).
pub(crate) unsafe fn create_search_box(hwnd: HWND) {
    let (hinst, font) =
        APP.with(|c| c.borrow().as_ref().map(|a| (a.hinst, a.font_small)).unwrap_or_default());
    let edit = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("EDIT"),
        PCWSTR::null(),
        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
        18, // слева оставлен «≡» как ручка перетаскивания панели
        2,
        render::W - 2 * render::HEAD_BTN_W - 22,
        render::HEAD - 4,
        hwnd,
        HMENU(ID_SEARCH as *mut core::ffi::c_void),
        hinst,
        None,
    )
    .unwrap_or_default();
    if edit.0.is_null() {
        return;
    }
    SendMessageW(edit, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
    // правый отступ под крестик очистки (текст не залезает под ✕)
    SendMessageW(edit, EM_SETMARGINS, WPARAM(EC_RIGHTMARGIN as usize), LPARAM((CLEAR_W << 16) as isize));
    let cue: Vec<u16> = "Поиск по чатам…".encode_utf16().chain(std::iter::once(0)).collect();
    SendMessageW(edit, EM_SETCUEBANNER, WPARAM(1), LPARAM(cue.as_ptr() as isize));
    // субкласс EDIT для перехвата Enter/Esc
    let old = SetWindowLongPtrW(edit, GWLP_WNDPROC, search_edit_proc as *const () as isize);
    SEARCH_OLDPROC.with(|p| p.set(old));
    // выпадающий список истории (child LISTBOX, скрыт до клика по полю)
    let list = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("LISTBOX"),
        PCWSTR::null(),
        WS_CHILD | WS_BORDER | WS_VSCROLL | WINDOW_STYLE(LBS_NOTIFY),
        0,
        0,
        0,
        0,
        hwnd,
        HMENU(ID_HIST_LIST as *mut core::ffi::c_void),
        hinst,
        None,
    )
    .unwrap_or_default();
    if !list.0.is_null() {
        SendMessageW(list, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
    }
    APP.with(|c| {
        if let Some(a) = c.borrow_mut().as_mut() {
            a.search_edit = edit;
            a.hist_list = list;
        }
    });
}

// WM_COMMAND от родителя: наши ли это id? true = обработали (main не зовёт handle_command).
pub(crate) unsafe fn on_command(hwnd: HWND, id: usize, notif: u32) -> bool {
    if id == ID_SEARCH {
        if notif == EN_CHANGE {
            run_live_search(hwnd);
        }
        return true;
    }
    if id == ID_HIST_LIST {
        if notif == LBN_SELCHANGE {
            let (edit, list) =
                APP.with(|c| c.borrow().as_ref().map(|a| (a.search_edit, a.hist_list)).unwrap_or_default());
            let sel = SendMessageW(list, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as i32;
            pick_history(edit, list, sel);
        }
        return true;
    }
    false
}

// WM_CTLCOLOREDIT: фон/текст поля поиска — светло-голубой, чтобы белый не резал на тёмном.
pub(crate) unsafe fn ctl_color_edit(hdc: HDC) -> isize {
    SetBkColor(hdc, COLORREF(C_SEARCH_BG));
    SetTextColor(hdc, COLORREF(C_SEARCH_TXT));
    let mut b = SEARCH_BRUSH.with(|c| c.get());
    if b == 0 {
        b = CreateSolidBrush(COLORREF(C_SEARCH_BG)).0 as isize;
        SEARCH_BRUSH.with(|c| c.set(b));
    }
    b
}

// Подсказка-заглушка поля поиска (используется как лёгкий индикатор индексации).
pub(crate) unsafe fn set_search_cue(text: PCWSTR) {
    let edit = APP.with(|c| c.borrow().as_ref().map(|a| a.search_edit).unwrap_or_default());
    if !edit.0.is_null() {
        SendMessageW(edit, EM_SETCUEBANNER, WPARAM(1), LPARAM(text.0 as isize));
    }
}

// Esc: очистить поиск (текст -> EN_CHANGE снимет подсветку); поле остаётся открытым.
unsafe fn clear_search() {
    let edit = APP.with(|c| c.borrow().as_ref().map(|a| a.search_edit).unwrap_or_default());
    if !edit.0.is_null() {
        record_history(&edit_text(edit)); // запомнить завершённый запрос перед очисткой
        let _ = SetWindowTextW(edit, w!(""));
    }
}

// Субкласс EDIT: Enter -> dense, Esc -> закрыть; прочее в старый proc.
extern "system" fn search_edit_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_KEYDOWN {
            let vk = wp.0 as u32;
            let list = APP.with(|c| c.borrow().as_ref().map(|a| a.hist_list).unwrap_or_default());
            let drop_open = !list.0.is_null() && IsWindowVisible(list).as_bool();
            if drop_open && (vk == VK_DOWN.0 as u32 || vk == VK_UP.0 as u32) {
                let cnt = SendMessageW(list, LB_GETCOUNT, WPARAM(0), LPARAM(0)).0 as i32;
                let cur = SendMessageW(list, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as i32;
                let next = if vk == VK_DOWN.0 as u32 { (cur + 1).min(cnt - 1) } else { (cur - 1).max(0) };
                SendMessageW(list, LB_SETCURSEL, WPARAM(next as usize), LPARAM(0));
                return LRESULT(0);
            }
            if vk == VK_RETURN.0 as u32 {
                if drop_open {
                    let sel = SendMessageW(list, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as i32;
                    if sel >= 0 {
                        pick_history(hwnd, list, sel);
                        return LRESULT(0);
                    }
                }
                commit_search_enter(GetParent(hwnd).unwrap_or_default());
                return LRESULT(0);
            }
            if vk == VK_ESCAPE.0 as u32 {
                if drop_open {
                    hide_history_dropdown();
                    return LRESULT(0);
                }
                clear_search();
                return LRESULT(0);
            }
        }
        if msg == WM_MOUSEMOVE {
            arm_tip(GetParent(hwnd).unwrap_or_default(), TIP_SEARCHBOX);
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut tme);
        } else if msg == WM_MOUSELEAVE {
            arm_tip(GetParent(hwnd).unwrap_or_default(), -1);
        }
        // над значком ✕/▾ — курсор-стрелка вместо I-beam
        if msg == WM_SETCURSOR {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = ScreenToClient(hwnd, &mut pt);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let has_icon = GetWindowTextLengthW(hwnd) > 0
                || APP.with(|c| c.borrow().as_ref().map(|a| !a.search_history.is_empty()).unwrap_or(false));
            if has_icon && pt.x >= rc.right - CLEAR_W {
                let _ = SetCursor(LoadCursorW(None, IDC_ARROW).unwrap_or_default());
                return LRESULT(1);
            }
        }
        // клик: ✕ (есть текст) очистить; пустое поле -> выпадающая история
        if msg == WM_LBUTTONDOWN {
            let x = (lp.0 & 0xFFFF) as i16 as i32;
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            if GetWindowTextLengthW(hwnd) > 0 {
                if x >= rc.right - CLEAR_W {
                    clear_search();
                    let _ = SetFocus(hwnd);
                    return LRESULT(0);
                }
            } else {
                show_history_dropdown(hwnd); // не return — дефолт поставит курсор
            }
        }
        // дорисовать значок поверх поля (✕ / ▾)
        if msg == WM_PAINT {
            let oldp: WNDPROC = std::mem::transmute::<isize, WNDPROC>(SEARCH_OLDPROC.with(|p| p.get()));
            let r = CallWindowProcW(oldp, hwnd, msg, wp, lp);
            draw_field_icon(hwnd);
            return r;
        }
        // потеря фокуса (но не на список истории): запомнить запрос + скрыть dropdown.
        // Клик по результату/окну забирает фокус с поля — это и есть «завершение поиска».
        if msg == WM_KILLFOCUS {
            let gaining = HWND(wp.0 as *mut core::ffi::c_void);
            let list = APP.with(|c| c.borrow().as_ref().map(|a| a.hist_list).unwrap_or_default());
            if gaining.0 != list.0 {
                record_history(&edit_text(hwnd));
                hide_history_dropdown();
            }
        }
        let old: WNDPROC = std::mem::transmute::<isize, WNDPROC>(SEARCH_OLDPROC.with(|p| p.get()));
        CallWindowProcW(old, hwnd, msg, wp, lp)
    }
}

// Значок в правом отступе поля: ✕ (есть текст -> очистить) или ▾ (пусто -> история). Две линии.
unsafe fn draw_field_icon(edit: HWND) {
    let has_text = GetWindowTextLengthW(edit) > 0;
    let has_hist = APP.with(|c| c.borrow().as_ref().map(|a| !a.search_history.is_empty()).unwrap_or(false));
    if !has_text && !has_hist {
        return; // пусто и истории нет — ничего не рисуем
    }
    let mut rc = RECT::default();
    let _ = GetClientRect(edit, &mut rc);
    let hdc = GetDC(edit);
    let cx = rc.right - CLEAR_W / 2;
    let cy = rc.bottom / 2;
    let pen = CreatePen(PS_SOLID, 1, COLORREF(C_SEARCH_TXT));
    let old = SelectObject(hdc, pen);
    if has_text {
        let s = 4; // ✕
        let _ = MoveToEx(hdc, cx - s, cy - s, None);
        let _ = LineTo(hdc, cx + s + 1, cy + s + 1);
        let _ = MoveToEx(hdc, cx - s, cy + s, None);
        let _ = LineTo(hdc, cx + s + 1, cy - s - 1);
    } else {
        let s = 4; // ▾ (шеврон вниз)
        let _ = MoveToEx(hdc, cx - s, cy - 2, None);
        let _ = LineTo(hdc, cx, cy + 3);
        let _ = MoveToEx(hdc, cx + s + 1, cy - 2, None);
        let _ = LineTo(hdc, cx, cy + 3);
    }
    SelectObject(hdc, old);
    let _ = DeleteObject(pen);
    ReleaseDC(edit, hdc);
}

// ---------- история поисков (Phase-13) ----------
fn history_file() -> std::path::PathBuf {
    let base = std::env::var_os("APPDATA").map(std::path::PathBuf::from).unwrap_or_default();
    base.join("claudebar").join("search_history.txt")
}

pub(crate) fn load_history() -> Vec<String> {
    std::fs::read_to_string(history_file())
        .map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

fn save_history(h: &[String]) {
    let p = history_file();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, h.join("\n"));
}

// Записать запрос в историю (дедуп, свежие первыми, лимит), сохранить.
pub(crate) fn record_history(query: &str) {
    let q = query.trim().to_string();
    if q.chars().count() < SEARCH_MIN {
        return;
    }
    APP.with(|c| {
        if let Some(a) = c.borrow_mut().as_mut() {
            a.search_history.retain(|h| h != &q);
            a.search_history.insert(0, q);
            a.search_history.truncate(HIST_MAX);
            save_history(&a.search_history);
        }
    });
}

// Показать выпадающий список истории под полем (немодальный child LISTBOX).
unsafe fn show_history_dropdown(edit: HWND) {
    let (list, hist) = APP.with(|c| {
        c.borrow().as_ref().map(|a| (a.hist_list, a.search_history.clone())).unwrap_or_default()
    });
    if list.0.is_null() || hist.is_empty() {
        return;
    }
    SendMessageW(list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
    for q in &hist {
        let w: Vec<u16> = q.encode_utf16().chain(std::iter::once(0)).collect();
        SendMessageW(list, LB_ADDSTRING, WPARAM(0), LPARAM(w.as_ptr() as isize));
    }
    let parent = GetParent(edit).unwrap_or_default();
    let mut er = RECT::default();
    let _ = GetWindowRect(edit, &mut er);
    let mut pt = POINT { x: er.left, y: er.bottom };
    let _ = ScreenToClient(parent, &mut pt);
    let w = er.right - er.left;
    let ih = SendMessageW(list, LB_GETITEMHEIGHT, WPARAM(0), LPARAM(0)).0 as i32;
    let ih = if ih > 0 { ih } else { 18 };
    let n = hist.len().min(8) as i32;
    let _ = SetWindowPos(list, HWND_TOP, pt.x, pt.y, w, n * ih + 4, SWP_NOACTIVATE | SWP_SHOWWINDOW);
}

pub(crate) unsafe fn hide_history_dropdown() {
    let list = APP.with(|c| c.borrow().as_ref().map(|a| a.hist_list).unwrap_or_default());
    if !list.0.is_null() {
        let _ = ShowWindow(list, SW_HIDE);
    }
}

// Подставить элемент истории (sel) в поле -> EN_CHANGE запустит поиск; вернуть фокус, скрыть.
unsafe fn pick_history(edit: HWND, list: HWND, sel: i32) {
    if sel < 0 {
        return;
    }
    let len = SendMessageW(list, LB_GETTEXTLEN, WPARAM(sel as usize), LPARAM(0)).0;
    if len <= 0 {
        return;
    }
    let mut buf = vec![0u16; len as usize + 1];
    SendMessageW(list, LB_GETTEXT, WPARAM(sel as usize), LPARAM(buf.as_mut_ptr() as isize));
    let _ = SetWindowTextW(edit, PCWSTR(buf.as_ptr()));
    let _ = SetFocus(edit);
    hide_history_dropdown();
}
