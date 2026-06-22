#![windows_subsystem = "windows"]
//! ClaudeBar — крошечная всегда-поверх панель для переключения между открытыми
//! окнами редакторов (VS Code / Cursor), в которых крутится Claude Code.
//! ЛКМ по строке — перейти в окно. ПКМ — задать цвет и метку. Привязка по имени проекта.

mod activate;
mod config;
mod icon;
mod recent;
mod render;
#[allow(dead_code)] // M-SEARCH/M-SDAEMON подключает step-5 (M-MAIN: окошко поиска)
mod sdaemon;
#[allow(dead_code)]
mod search;
mod settings;
mod signal;
mod win_enum;

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;

use config::{Config, PALETTE};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, ReleaseCapture, SetCapture, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

const EM_SETSEL: u32 = 0x00B1;

// id команд меню
const ID_COLOR_BASE: usize = 1; // 1..=8
const ID_LABEL: usize = 20;
const ID_LABEL_CLEAR: usize = 21;
const ID_SET_FONT: usize = 30; // меню настроек: выбрать шрифт
const ID_ABOUT: usize = 31; // меню настроек: о программе

// ---------- состояние ----------
pub(crate) struct App {
    pub(crate) hinst: HINSTANCE,
    pub(crate) items: Vec<win_enum::WinItem>,
    pub(crate) recent: Vec<recent::RecentDoc>,
    pub(crate) rows: Vec<render::Row>,
    pub(crate) config: Config,
    pub(crate) font_main: HFONT,
    pub(crate) font_small: HFONT,
    pub(crate) hover: i32,
    pub(crate) menu_target: usize, // индекс строки, по которой открыли меню
    pub(crate) last_h: i32,
    pub(crate) bell: HashSet<String>, // имена проектов со «звоночком» (lower) — подсветка строк
    pub(crate) search_hits: Vec<search::FolderHit>, // папки-совпадения поиска (Phase-12)
    pub(crate) reorder: bool, // режим перетаскивания: ручки видны, ✕ скрыт
    pub(crate) drag: Option<i32>, // индекс перетаскиваемой строки во время drag
}

thread_local! {
    static APP: RefCell<Option<App>> = RefCell::new(None);
}

// ---------- перечисление окон ----------
fn refresh_items(app: &mut App) {
    let raw = win_enum::list_windows();
    app.items = win_enum::match_windows(&raw, &app.config.apps);
    // открытые сейчас документы (basename без расширения, lower) — исключаем из недавних
    let open: HashSet<String> = app
        .items
        .iter()
        .map(|it| {
            let n = &it.name;
            n.rsplit_once('.').map(|(b, _)| b).unwrap_or(n).to_lowercase()
        })
        .collect();
    app.recent = recent::list_recent(&app.config.apps, &open);
    app.rows = render::build_rows(&app.items, &app.recent, &app.config.apps, &app.config);
    // звоночек: сбросить сигналы окон, получивших фокус, затем собрать активные ключи
    let fg = unsafe { GetForegroundWindow() };
    signal::reconcile(&app.items, fg);
    app.bell = signal::bell_keys();
}

// ---------- ввод метки (модальный prompt) ----------
thread_local! {
    static PROMPT_RESULT: RefCell<Option<String>> = RefCell::new(None);
    static PROMPT_EDIT: RefCell<HWND> = RefCell::new(HWND(std::ptr::null_mut()));
}

extern "system" fn prompt_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COMMAND => {
                let id = (wp.0 & 0xFFFF) as usize;
                if id == 1 {
                    // OK
                    let edit = PROMPT_EDIT.with(|e| *e.borrow());
                    let len = GetWindowTextLengthW(edit);
                    let mut buf = vec![0u16; (len + 1) as usize];
                    let n = GetWindowTextW(edit, &mut buf);
                    let s = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
                    PROMPT_RESULT.with(|r| *r.borrow_mut() = Some(s));
                    let _ = DestroyWindow(hwnd);
                    return LRESULT(0);
                } else if id == 2 {
                    let _ = DestroyWindow(hwnd);
                    return LRESULT(0);
                }
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

fn prompt_text(parent: HWND, hinst: HINSTANCE, initial: &str) -> Option<String> {
    unsafe {
        PROMPT_RESULT.with(|r| *r.borrow_mut() = None);
        let cls = w!("claudebar_prompt");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(prompt_proc),
            hInstance: hinst,
            lpszClassName: cls,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_3DFACE),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let mut pr = RECT::default();
        let _ = GetWindowRect(parent, &mut pr);
        let dw = 320;
        let dh = 132;
        let x = pr.left + 10;
        let y = pr.top + 10;

        let dlg = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_DLGMODALFRAME,
            cls,
            w!("Метка для проекта"),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            x,
            y,
            dw,
            dh,
            parent,
            None,
            hinst,
            None,
        )
        .unwrap_or_default();
        if dlg.0.is_null() {
            return None;
        }

        let init: Vec<u16> = initial.encode_utf16().chain(std::iter::once(0)).collect();
        let edit = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            w!("EDIT"),
            PCWSTR(init.as_ptr()),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            12,
            14,
            dw - 36,
            24,
            dlg,
            None,
            hinst,
            None,
        )
        .unwrap_or_default();
        PROMPT_EDIT.with(|e| *e.borrow_mut() = edit);

        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("OK"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            dw - 200,
            52,
            86,
            28,
            dlg,
            HMENU(1isize as *mut core::ffi::c_void),
            hinst,
            None,
        );
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("Отмена"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            dw - 106,
            52,
            86,
            28,
            dlg,
            HMENU(2isize as *mut core::ffi::c_void),
            hinst,
            None,
        );

        let _ = ShowWindow(dlg, SW_SHOW);
        let _ = SetFocus(edit);
        SendMessageW(edit, EM_SETSEL, WPARAM(0), LPARAM(-1));

        // локальный модальный цикл
        let _ = EnableWindow(parent, false);
        let mut msg = MSG::default();
        while IsWindow(dlg).as_bool() {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 <= 0 {
                break;
            }
            if !IsDialogMessageW(dlg, &mut msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let _ = EnableWindow(parent, true);
        let _ = SetForegroundWindow(parent);
        PROMPT_RESULT.with(|r| r.borrow_mut().take())
    }
}

// ---------- оконная процедура ----------
extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TIMER => {
                APP.with(|c| {
                    if let Some(app) = c.borrow_mut().as_mut() {
                        refresh_items(app);
                        render::resize(hwnd, app);
                    }
                });
                let _ = InvalidateRect(hwnd, None, BOOL(0));
                LRESULT(0)
            }
            WM_PAINT => {
                APP.with(|c| {
                    if let Some(app) = c.borrow().as_ref() {
                        render::paint(hwnd, app);
                    }
                });
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (_, y) = xy(lp);
                let new = APP.with(|c| {
                    c.borrow()
                        .as_ref()
                        .map(|a| render::row_at(y, a.rows.len()))
                        .unwrap_or(-1)
                });
                let changed = APP.with(|c| {
                    if let Some(a) = c.borrow_mut().as_mut() {
                        if a.hover != new {
                            a.hover = new;
                            return true;
                        }
                    }
                    false
                });
                if changed {
                    let _ = InvalidateRect(hwnd, None, BOOL(0));
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let (x, y) = xy(lp);
                if y < render::HEAD {
                    let w = client_w(hwnd);
                    if x >= w - render::HEAD_BTN_W {
                        let _ = DestroyWindow(hwnd);
                    } else if x >= w - 2 * render::HEAD_BTN_W {
                        show_settings_menu(hwnd);
                    } else {
                        // тянем панель за шапку
                        let _ = ReleaseCapture();
                        SendMessageW(hwnd, WM_NCLBUTTONDOWN, WPARAM(HTCAPTION as usize), LPARAM(0));
                    }
                    return LRESULT(0);
                }
                // режим reorder: начать перетаскивание за ручку
                let reorder = APP.with(|c| c.borrow().as_ref().map(|a| a.reorder).unwrap_or(false));
                if reorder {
                    let w = client_w(hwnd);
                    let start = APP.with(|c| {
                        let a = c.borrow();
                        let a = a.as_ref()?;
                        let (i, zone) = render::hit_test(x, y, &a.rows, w, true);
                        if i >= 0 && zone == render::Zone::DragHandle {
                            Some(i)
                        } else {
                            None
                        }
                    });
                    if let Some(i) = start {
                        SetCapture(hwnd);
                        APP.with(|c| {
                            if let Some(a) = c.borrow_mut().as_mut() {
                                a.drag = Some(i);
                                a.hover = i;
                            }
                        });
                        let _ = InvalidateRect(hwnd, None, BOOL(0));
                    }
                    return LRESULT(0);
                }
                enum Act {
                    Activate(HWND),
                    Close(HWND),
                    Toggle(usize),
                    ToggleRecent(usize),
                    ToggleShowall(usize),
                    Open(usize),
                }
                let w = client_w(hwnd);
                let act = APP.with(|c| {
                    let a = c.borrow();
                    let a = a.as_ref()?;
                    let (i, zone) = render::hit_test(x, y, &a.rows, w, false);
                    if i < 0 {
                        return None;
                    }
                    match a.rows[i as usize] {
                        render::Row::Window { idx } => {
                            let hwnd = a.items[idx].hwnd;
                            if zone == render::Zone::Close {
                                Some(Act::Close(hwnd))
                            } else {
                                Some(Act::Activate(hwnd))
                            }
                        }
                        render::Row::Section { app } => Some(Act::Toggle(app)),
                        render::Row::RecentHeader { app } => Some(Act::ToggleRecent(app)),
                        render::Row::Recent { ridx } => Some(Act::Open(ridx)),
                        render::Row::RecentMore { app } => Some(Act::ToggleShowall(app)),
                        render::Row::SearchHeader => None,
                        render::Row::SearchResult { .. } => None, // открытие — step-5 (M-MAIN)
                    }
                });
                #[derive(Clone, Copy)]
                enum SecToggle {
                    Collapse,
                    Recent,
                    Showall,
                }
                let toggle_section = |sec: usize, kind: SecToggle| {
                    APP.with(|c| {
                        if let Some(a) = c.borrow_mut().as_mut() {
                            let block = a.config.apps[sec].block.clone();
                            match kind {
                                SecToggle::Collapse => a.config.toggle_collapsed(&block),
                                SecToggle::Recent => a.config.toggle_recent(&block),
                                SecToggle::Showall => a.config.toggle_showall(&block),
                            }
                            a.config.save(hwnd);
                            a.rows = render::build_rows(&a.items, &a.recent, &a.config.apps, &a.config);
                            render::resize(hwnd, a);
                        }
                    });
                    let _ = InvalidateRect(hwnd, None, BOOL(0));
                };
                match act {
                    Some(Act::Activate(t)) => activate::activate(t),
                    Some(Act::Close(t)) => activate::close(t),
                    Some(Act::Toggle(sec)) => toggle_section(sec, SecToggle::Collapse),
                    Some(Act::ToggleRecent(sec)) => toggle_section(sec, SecToggle::Recent),
                    Some(Act::ToggleShowall(sec)) => toggle_section(sec, SecToggle::Showall),
                    Some(Act::Open(ridx)) => {
                        let cmd = APP.with(|c| {
                            c.borrow().as_ref().and_then(|a| a.recent.get(ridx).map(|d| d.open.clone()))
                        });
                        if let Some(cmd) = cmd {
                            recent::open(&cmd);
                        }
                    }
                    None => {}
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let dragging = APP.with(|c| c.borrow().as_ref().and_then(|a| a.drag));
                if let Some(from) = dragging {
                    let (_, y) = xy(lp);
                    let _ = ReleaseCapture();
                    APP.with(|c| {
                        if let Some(a) = c.borrow_mut().as_mut() {
                            let to = render::row_at(y, a.rows.len());
                            drop_reorder(a, from, to);
                            a.drag = None;
                            a.config.save(hwnd);
                            a.rows = render::build_rows(&a.items, &a.recent, &a.config.apps, &a.config);
                            render::resize(hwnd, a);
                        }
                    });
                    let _ = InvalidateRect(hwnd, None, BOOL(0));
                }
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                let (_, y) = xy(lp);
                enum R {
                    Menu(usize),
                    ToggleReorder,
                }
                let act = APP.with(|c| {
                    let a = c.borrow();
                    let a = a.as_ref()?;
                    let i = render::row_at(y, a.rows.len());
                    if i < 0 {
                        return None;
                    }
                    match a.rows[i as usize] {
                        render::Row::Window { idx } => Some(R::Menu(idx)),
                        render::Row::Section { .. } => Some(R::ToggleReorder),
                        _ => None,
                    }
                });
                match act {
                    Some(R::Menu(wi)) => {
                        APP.with(|c| {
                            if let Some(a) = c.borrow_mut().as_mut() {
                                a.menu_target = wi;
                            }
                        });
                        show_menu(hwnd);
                    }
                    Some(R::ToggleReorder) => {
                        APP.with(|c| {
                            if let Some(a) = c.borrow_mut().as_mut() {
                                a.reorder = !a.reorder;
                                a.drag = None;
                            }
                        });
                        let _ = InvalidateRect(hwnd, None, BOOL(0));
                    }
                    None => {}
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                let id = (wp.0 & 0xFFFF) as usize;
                handle_command(hwnd, id);
                LRESULT(0)
            }
            WM_DESTROY => {
                APP.with(|c| {
                    if let Some(app) = c.borrow().as_ref() {
                        app.config.save(hwnd);
                        let _ = DeleteObject(app.font_main);
                        let _ = DeleteObject(app.font_small);
                    }
                });
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn xy(lp: LPARAM) -> (i32, i32) {
    let x = (lp.0 & 0xFFFF) as i16 as i32;
    let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
    (x, y)
}

fn client_w(hwnd: HWND) -> i32 {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    rc.right
}

// Применить перетаскивание строки from на позицию to: переставить секцию или окно.
fn drop_reorder(a: &mut App, from: i32, to: i32) {
    if from < 0 {
        return;
    }
    let rows = a.rows.clone();
    let from = from as usize;
    if from >= rows.len() {
        return;
    }
    let to_idx = if to < 0 {
        rows.len().saturating_sub(1)
    } else {
        (to as usize).min(rows.len().saturating_sub(1))
    };
    match rows[from] {
        render::Row::Section { app: fa } => {
            let blocks = render::section_blocks(&rows, &a.config.apps);
            let from_block = a.config.apps[fa].block.clone();
            if let Some(ta) = render::section_of_row(&rows, to_idx) {
                let to_block = a.config.apps[ta].block.clone();
                if let (Some(fi), Some(ti)) = (
                    blocks.iter().position(|b| *b == from_block),
                    blocks.iter().position(|b| *b == to_block),
                ) {
                    a.config.move_section(&blocks, fi, ti);
                }
            }
        }
        render::Row::Window { idx: fidx } => {
            let fa = a.items[fidx].app;
            // переставляем только в пределах той же секции
            if render::section_of_row(&rows, to_idx) == Some(fa) {
                let names = render::window_names(&rows, &a.items, fa);
                let from_name = a.items[fidx].name.clone();
                let to_name = match rows[to_idx] {
                    render::Row::Window { idx } if a.items[idx].app == fa => Some(a.items[idx].name.clone()),
                    _ => None,
                };
                let block = a.config.apps[fa].block.clone();
                if let Some(fi) = names.iter().position(|n| *n == from_name) {
                    let ti = match to_name {
                        Some(tn) => names.iter().position(|n| *n == tn).unwrap_or(fi),
                        None => names.len().saturating_sub(1),
                    };
                    a.config.move_window(&block, &names, fi, ti);
                }
            }
        }
        _ => {}
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let menu = CreatePopupMenu().unwrap_or_default();
    for (i, p) in PALETTE.iter().enumerate() {
        let name: Vec<u16> = p.0.encode_utf16().chain(std::iter::once(0)).collect();
        let _ = AppendMenuW(menu, MF_STRING, ID_COLOR_BASE + i, PCWSTR(name.as_ptr()));
    }
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
    let _ = AppendMenuW(menu, MF_STRING, ID_LABEL, w!("Метка…"));
    let _ = AppendMenuW(menu, MF_STRING, ID_LABEL_CLEAR, w!("Убрать метку"));
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(menu, TPM_LEFTALIGN | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
    let _ = DestroyMenu(menu);
}

// Меню настроек панели (вызывается кликом «⚙» в шапке): выбор шрифта и «О программе».
unsafe fn show_settings_menu(hwnd: HWND) {
    let menu = CreatePopupMenu().unwrap_or_default();
    let _ = AppendMenuW(menu, MF_STRING, ID_SET_FONT, w!("Шрифт…"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
    let _ = AppendMenuW(menu, MF_STRING, ID_ABOUT, w!("О программе…"));
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(menu, TPM_LEFTALIGN | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
    let _ = DestroyMenu(menu);
}

fn handle_command(hwnd: HWND, id: usize) {
    // настройки: выбрать шрифт (не привязано к проекту)
    if id == ID_SET_FONT {
        let cur = APP.with(|c| c.borrow().as_ref().map(|a| (a.config.font_face.clone(), a.config.font_size, a.config.font_weight)));
        if let Some((face, size, weight)) = cur {
            // диалог модальный — borrow APP не держим, пока он открыт
            if let Some((nf, ns, nw)) = settings::choose_font(hwnd, &face, size, weight) {
                APP.with(|c| {
                    if let Some(a) = c.borrow_mut().as_mut() {
                        a.config.set_font(&nf, ns, nw);
                        a.config.save(hwnd);
                        rebuild_fonts(a);
                    }
                });
                unsafe {
                    let _ = InvalidateRect(hwnd, None, BOOL(0));
                }
            }
        }
        return;
    }
    // настройки: окно «О программе» (версия + контакты автора)
    if id == ID_ABOUT {
        settings::show_about(hwnd);
        return;
    }
    // имя проекта по menu_target
    let project = APP.with(|c| {
        let a = c.borrow();
        let a = a.as_ref()?;
        a.items.get(a.menu_target).map(|it| it.name.clone())
    });
    let Some(project) = project else { return };

    if (ID_COLOR_BASE..ID_COLOR_BASE + PALETTE.len()).contains(&id) {
        APP.with(|c| {
            if let Some(a) = c.borrow_mut().as_mut() {
                a.config.set_color(&project, id - ID_COLOR_BASE);
                a.config.save(hwnd);
            }
        });
        unsafe {
            let _ = InvalidateRect(hwnd, None, BOOL(0));
        }
    } else if id == ID_LABEL {
        let Some((hinst, cur)) = APP.with(|c| {
            let a = c.borrow();
            let a = a.as_ref()?;
            Some((a.hinst, a.config.label(&project)))
        }) else { return };
        if let Some(s) = prompt_text(hwnd, hinst, &cur) {
            APP.with(|c| {
                if let Some(a) = c.borrow_mut().as_mut() {
                    a.config.set_label(&project, s.trim().to_string());
                    a.config.save(hwnd);
                }
            });
            unsafe {
                let _ = InvalidateRect(hwnd, None, BOOL(0));
            }
        }
    } else if id == ID_LABEL_CLEAR {
        APP.with(|c| {
            if let Some(a) = c.borrow_mut().as_mut() {
                a.config.set_label(&project, String::new());
                a.config.save(hwnd);
            }
        });
        unsafe {
            let _ = InvalidateRect(hwnd, None, BOOL(0));
        }
    }
}

// ---------- инициализация ----------
fn make_font(face: &str, height: i32, weight: i32) -> HFONT {
    let face: Vec<u16> = face.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            PCWSTR(face.as_ptr()),
        )
    }
}

// Пересоздать шрифты панели из конфигурации (после смены шрифта в настройках).
fn rebuild_fonts(app: &mut App) {
    unsafe {
        let _ = DeleteObject(app.font_main);
        let _ = DeleteObject(app.font_small);
    }
    let face = app.config.font_face.clone();
    let size = app.config.font_size;
    let weight = app.config.font_weight;
    app.font_main = make_font(&face, -size, weight);
    app.font_small = make_font(&face, -((size - 3).max(8)), weight.min(400));
}

fn main() -> Result<()> {
    unsafe {
        let hmod = GetModuleHandleW(None)?;
        let hinst = HINSTANCE(hmod.0);

        let exe = std::env::current_exe().unwrap_or_default();
        let cfg_path = exe
            .parent()
            .map(|p| p.join("claudebar.ini"))
            .unwrap_or_else(|| PathBuf::from("claudebar.ini"));
        let config = Config::load(cfg_path);
        let font_face = config.font_face.clone();
        let font_size = config.font_size;
        let font_weight = config.font_weight;

        let mut app = App {
            hinst,
            items: Vec::new(),
            recent: Vec::new(),
            rows: Vec::new(),
            config,
            font_main: make_font(&font_face, -font_size, font_weight),
            font_small: make_font(&font_face, -((font_size - 3).max(8)), font_weight.min(400)),
            hover: -1,
            menu_target: 0,
            last_h: 0,
            bell: HashSet::new(),
            search_hits: Vec::new(),
            reorder: false,
            drag: None,
        };
        refresh_items(&mut app);

        let cls = w!("claudebar_wnd");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: cls,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);

        // позиция: из конфига, но только если окно реально видно на текущей конфигурации
        // мониторов. После отключения/перестановки монитора сохранённая позиция может
        // оказаться вне виртуального экрана — тогда окно невидимо (висит лишь в панели задач);
        // visible_start_pos в этом случае возвращает дефолт на первичном экране.
        let sw = GetSystemMetrics(SM_CXSCREEN);
        let n = app.rows.len().max(1) as i32;
        let h = render::HEAD + render::ROW * n;
        let default_pos = (sw - render::W - 20, 40);
        let (vx, vy, vw, vh) = (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        );
        let (x, y) =
            config::visible_start_pos(app.config.pos, default_pos, render::W, h, vx, vy, vw, vh);

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            cls,
            w!("ClaudeBar"),
            WS_POPUP,
            x,
            y,
            render::W,
            h,
            None,
            None,
            hinst,
            None,
        )?;
        app.last_h = h;

        APP.with(|c| *c.borrow_mut() = Some(app));

        let _ = ShowWindow(hwnd, SW_SHOW);
        SetTimer(hwnd, 1, 1000, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}
