#![windows_subsystem = "windows"]
//! ClaudeBar — крошечная всегда-поверх панель для переключения между открытыми
//! окнами редакторов (VS Code / Cursor), в которых крутится Claude Code.
//! ЛКМ по строке — перейти в окно. ПКМ — задать цвет и метку. Привязка по имени проекта.

mod activate;
mod config;
mod icon;
mod recent;
mod render;
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
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, ReleaseCapture, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

const EM_SETSEL: u32 = 0x00B1;

// id команд меню
const ID_COLOR_BASE: usize = 1; // 1..=8
const ID_LABEL: usize = 20;
const ID_LABEL_CLEAR: usize = 21;

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
                    if x > w - 24 {
                        let _ = DestroyWindow(hwnd);
                    } else {
                        // тянем панель за шапку
                        let _ = ReleaseCapture();
                        SendMessageW(hwnd, WM_NCLBUTTONDOWN, WPARAM(HTCAPTION as usize), LPARAM(0));
                    }
                    return LRESULT(0);
                }
                enum Act {
                    Activate(HWND),
                    Close(HWND),
                    Toggle(usize),
                    ToggleRecent(usize),
                    Open(usize),
                }
                let w = client_w(hwnd);
                let act = APP.with(|c| {
                    let a = c.borrow();
                    let a = a.as_ref()?;
                    let (i, zone) = render::hit_test(x, y, &a.rows, w);
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
                    }
                });
                let toggle_section = |sec: usize, recent_cut: bool| {
                    APP.with(|c| {
                        if let Some(a) = c.borrow_mut().as_mut() {
                            let block = a.config.apps[sec].block.clone();
                            if recent_cut {
                                a.config.toggle_recent(&block);
                            } else {
                                a.config.toggle_collapsed(&block);
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
                    Some(Act::Toggle(sec)) => toggle_section(sec, false),
                    Some(Act::ToggleRecent(sec)) => toggle_section(sec, true),
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
            WM_RBUTTONUP => {
                let (_, y) = xy(lp);
                let witem = APP.with(|c| {
                    let a = c.borrow();
                    let a = a.as_ref()?;
                    let i = render::row_at(y, a.rows.len());
                    if i < 0 {
                        return None;
                    }
                    match a.rows[i as usize] {
                        render::Row::Window { idx } => Some(idx),
                        _ => None,
                    }
                });
                if let Some(wi) = witem {
                    APP.with(|c| {
                        if let Some(a) = c.borrow_mut().as_mut() {
                            a.menu_target = wi;
                        }
                    });
                    show_menu(hwnd);
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

fn handle_command(hwnd: HWND, id: usize) {
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
fn make_font(height: i32, weight: i32) -> HFONT {
    let face: Vec<u16> = "Segoe UI".encode_utf16().chain(std::iter::once(0)).collect();
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

        let mut app = App {
            hinst,
            items: Vec::new(),
            recent: Vec::new(),
            rows: Vec::new(),
            config,
            font_main: make_font(-16, 600),
            font_small: make_font(-13, 400),
            hover: -1,
            menu_target: 0,
            last_h: 0,
            bell: HashSet::new(),
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

        // позиция: из конфига или правый верхний угол
        let sw = GetSystemMetrics(SM_CXSCREEN);
        let n = app.rows.len().max(1) as i32;
        let h = render::HEAD + render::ROW * n;
        let (x, y) = app.config.pos.unwrap_or((sw - render::W - 20, 40));

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
