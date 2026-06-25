#![windows_subsystem = "windows"]
//! ClaudeBar — крошечная всегда-поверх панель для переключения между открытыми
//! окнами редакторов (VS Code / Cursor), в которых крутится Claude Code.
//! ЛКМ по строке — перейти в окно. ПКМ — задать цвет и метку. Привязка по имени проекта.

mod activate;
mod config;
mod icon;
mod index;
mod recent;
mod render;
#[allow(dead_code)] // dormant: dense отложен (Phase-13); M-SDAEMON оживёт с Python-модулем смысла
mod sdaemon;
#[allow(dead_code)] // dense-помощники dormant + snippet_for ждёт Ф-B (тултипы)
mod search;
mod settings;
mod signal;
mod win_enum;

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use config::{Config, PALETTE};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE,
    TRACKMOUSEEVENT, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_UP,
};
use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellExecuteW, ShellLink};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::WindowsAndMessaging::*;
use std::os::windows::ffi::OsStrExt;

const EM_SETSEL: u32 = 0x00B1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const ID_TIP_TIMER: usize = 3; // dwell-таймер подсказки (~0.5с)
const TIP_DELAY: u32 = 500; // мс выдержки перед показом подсказки
const TIP_SEARCHBOX: i32 = -2; // tip_row: курсор над строкой поиска -> правила
const C_TIP_BG: u32 = 0x00E1FFFF; // фон подсказки (инфо-жёлтый, RGB 255,255,225)
const C_TIP_TXT: u32 = 0x00202020; // тёмный текст подсказки
const SEARCH_RULES: &str = "Правила поиска:\r\n  пробел — И (оба слова)\r\n  a+b — точная фраза (подряд)\r\n  a++b — рядом (NEAR)\r\n  -слово — исключить\r\n  OR — или (заглавными)\r\nIP / путь / дата ищутся как фраза.";

thread_local! {
    // текст текущей подсказки (для собственной отрисовки попапа)
    static TIP_SHOW: RefCell<String> = const { RefCell::new(String::new()) };
}

// id команд меню
const ID_COLOR_BASE: usize = 1; // 1..=8
const ID_LABEL: usize = 20;
const ID_LABEL_CLEAR: usize = 21;
const ID_COPY_LINK: usize = 22; // меню окна: скопировать путь (Phase-14)
const ID_OPEN_DIR: usize = 23; // меню окна: открыть в проводнике (Phase-14)
const CF_UNICODETEXT: u32 = 13; // формат буфера обмена Win32 (clipboard CF_UNICODETEXT)

// Пока открыто контекстное меню (модальный TrackPopupMenu) — WM_TIMER не трогает окно/тултип,
// иначе любой тик (dwell-тултип ~0.5с или общий refresh ~1с) закрывает меню. Phase-14 fix.
static MENU_ACTIVE: AtomicBool = AtomicBool::new(false);
const ID_SET_FONT: usize = 30; // меню настроек: выбрать шрифт
const ID_ABOUT: usize = 31; // меню настроек: о программе
const ID_TOGGLE_FILES: usize = 32; // меню настроек: искать и в файлах (history)
const ID_SEARCH: usize = 40; // EDIT-поле поиска в шапке (WM_COMMAND EN_CHANGE)
const SEARCH_MIN: usize = 3; // живой BM25 начинается с N символов
const WM_APP_SEARCH: u32 = WM_APP + 1; // dense-результаты из фонового потока
const EM_SETCUEBANNER: u32 = 0x1501; // подсказка-заглушка в пустом EDIT
const EM_SETMARGINS: u32 = 0x00D3;
const EC_RIGHTMARGIN: u32 = 0x0002;
const CLEAR_W: i32 = 18; // зона значка справа в поле поиска (✕ очистка / ▾ история)
const HIST_MAX: usize = 15; // лимит истории поисков
const ID_HIST_LIST: usize = 41; // id child-LISTBOX выпадающей истории
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
    pub(crate) menu_link: Option<(String, bool)>, // цель «ссылка/проводник»: (путь, is_file) — Phase-14
    pub(crate) last_h: i32,
    pub(crate) bell: HashSet<String>, // имена проектов со «звоночком» (lower) — подсветка строк
    pub(crate) search_hits: Vec<search::FolderHit>, // папки-совпадения поиска (Phase-12)
    pub(crate) search_edit: HWND, // EDIT-поле поиска в шапке (null = скрыто)
    pub(crate) tooltip: HWND, // tracking-подсказка (путь/сниппет/правила) — Phase-13 Ф-B
    pub(crate) tip_row: i32, // под подсказкой: -1 нет, -2 строка поиска, >=0 индекс строки
    pub(crate) search_history: Vec<String>, // недавние запросы (свежие первыми) — Phase-13
    pub(crate) hist_list: HWND, // выпадающий список истории (child LISTBOX, скрыт)
    pub(crate) reorder: bool, // режим перетаскивания: ручки видны, ✕ скрыт
    pub(crate) drag: Option<i32>, // индекс перетаскиваемой строки во время drag
}

thread_local! {
    static APP: RefCell<Option<App>> = RefCell::new(None);
}

thread_local! {
    // старый WNDPROC EDIT-поля поиска (для субкласса Enter/Esc)
    static SEARCH_OLDPROC: Cell<isize> = const { Cell::new(0) };
}

thread_local! {
    // кисть фона поля поиска (создаётся один раз)
    static SEARCH_BRUSH: Cell<isize> = const { Cell::new(0) };
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
    rebuild_rows(app);
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
                if MENU_ACTIVE.load(Ordering::Relaxed) {
                    // контекстное меню открыто: не показываем тултип и не перерисовываем,
                    // иначе модальный TrackPopupMenu закрывается на первом же тике
                    return LRESULT(0);
                }
                if wp.0 == ID_TIP_TIMER {
                    let _ = KillTimer(hwnd, ID_TIP_TIMER);
                    show_tooltip(hwnd);
                    return LRESULT(0);
                }
                APP.with(|c| {
                    if let Some(app) = c.borrow_mut().as_mut() {
                        refresh_items(app);
                        render::resize(hwnd, app);
                    }
                });
                let _ = InvalidateRect(hwnd, None, BOOL(0));
                // фоновая переиндексация раз в ~3 мин
                let t = INDEX_TICKS.fetch_add(1, Ordering::Relaxed);
                if t > 0 && t % INDEX_EVERY_TICKS == 0 {
                    spawn_index(hwnd);
                    spawn_doc_paths(); // освежить пути документов (Phase-14)
                }
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
                arm_tip(hwnd, new);
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                arm_tip(hwnd, -1);
                let was = APP.with(|c| {
                    c.borrow_mut()
                        .as_mut()
                        .map(|a| {
                            let w = a.hover != -1;
                            a.hover = -1;
                            w
                        })
                        .unwrap_or(false)
                });
                if was {
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
                        // тянем панель за шапку («≡» слева и зазоры; поле поиска ловит свои клики)
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
                    OpenFolder(String),
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
                        render::Row::SearchResult { hit } => {
                            a.search_hits.get(hit).map(|h| Act::OpenFolder(h.folder.clone()))
                        }
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
                            rebuild_rows(a);
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
                    Some(Act::OpenFolder(folder)) => {
                        let wide: Vec<u16> = folder.encode_utf16().chain(std::iter::once(0)).collect();
                        ShellExecuteW(None, w!("open"), PCWSTR(wide.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
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
                            rebuild_rows(a);
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
                                a.menu_link = menu_link_for(a, wi); // путь для «ссылка/проводник» (Phase-14)
                            }
                        });
                        show_menu(hwnd);
                    }
                    Some(R::ToggleReorder) => {
                        APP.with(|c| {
                            if let Some(a) = c.borrow_mut().as_mut() {
                                a.reorder = !a.reorder;
                                a.drag = None;
                                // в режиме reorder прячем поле поиска (под ним подсказка)
                                let _ = ShowWindow(a.search_edit, if a.reorder { SW_HIDE } else { SW_SHOW });
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
                let notif = ((wp.0 >> 16) & 0xFFFF) as u32;
                if id == ID_SEARCH {
                    if notif == EN_CHANGE {
                        run_live_search(hwnd);
                    }
                } else if id == ID_HIST_LIST {
                    if notif == LBN_SELCHANGE {
                        let (edit, list) = APP.with(|c| {
                            c.borrow().as_ref().map(|a| (a.search_edit, a.hist_list)).unwrap_or_default()
                        });
                        let sel = SendMessageW(list, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as i32;
                        pick_history(edit, list, sel);
                    }
                } else {
                    handle_command(hwnd, id);
                }
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
            WM_CTLCOLOREDIT => {
                // фон/текст поля поиска — светло-голубой, чтобы белый не резал на тёмном
                let hdc = HDC(wp.0 as *mut core::ffi::c_void);
                SetBkColor(hdc, COLORREF(C_SEARCH_BG));
                SetTextColor(hdc, COLORREF(C_SEARCH_TXT));
                let mut b = SEARCH_BRUSH.with(|c| c.get());
                if b == 0 {
                    b = CreateSolidBrush(COLORREF(C_SEARCH_BG)).0 as isize;
                    SEARCH_BRUSH.with(|c| c.set(b));
                }
                LRESULT(b)
            }
            m if m == WM_APP_SEARCH => {
                // индекс готов -> вернуть обычную подсказку и перезапросить поиск
                set_search_cue(w!("Поиск по чатам…"));
                run_live_search(hwnd);
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

// ---------- поиск по чатам (Phase-12) ----------
fn edit_text(edit: HWND) -> String {
    if edit.0.is_null() {
        return String::new();
    }
    unsafe {
        let len = GetWindowTextLengthW(edit);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        let n = GetWindowTextW(edit, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

// Пересобрать строки панели + (если активен поиск) дописать блок «Найдено ещё».
fn rebuild_rows(app: &mut App) {
    app.rows = render::build_rows(&app.items, &app.recent, &app.config.apps, &app.config);
    if !app.search_hits.is_empty() {
        let open: HashSet<String> = app.items.iter().map(|it| it.name.to_lowercase()).collect();
        app.rows.extend(render::search_result_rows(&app.search_hits, &open));
    }
}

// Создать постоянное окошко поиска в шапке (один раз при старте).
unsafe fn create_search_box(hwnd: HWND) {
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

// Подсказка-заглушка поля поиска (используется как лёгкий индикатор индексации).
unsafe fn set_search_cue(text: PCWSTR) {
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

fn load_history() -> Vec<String> {
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
fn record_history(query: &str) {
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

unsafe fn hide_history_dropdown() {
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

// ---------- подсказки (tooltip, Phase-13 Ф-B) ----------

// Создать собственное popup-окно подсказки (один раз при старте).
unsafe fn create_tooltip(hwnd: HWND) {
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
unsafe fn arm_tip(hwnd: HWND, row: i32) {
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
unsafe fn show_tooltip(_hwnd: HWND) {
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

unsafe fn hide_tooltip(_hwnd: HWND) {
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
    match app.rows.get(tip_row as usize)? {
        render::Row::Window { idx } => app.items.get(*idx).map(|it| {
            // совпавшее с поиском открытое окно -> полный путь папки из хита, иначе имя
            let proj = it.name.to_lowercase();
            app.search_hits
                .iter()
                .find(|h| tip_basename(&h.folder) == proj)
                .map(|h| h.folder.clone())
                .unwrap_or_else(|| it.name.clone())
        }),
        render::Row::Recent { ridx } => app.recent.get(*ridx).map(recent_path),
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

fn recent_path(d: &recent::RecentDoc) -> String {
    match &d.open {
        recent::OpenCmd::Lnk(p) => p.display().to_string(),
        recent::OpenCmd::Editor { folder, .. } => folder.clone(),
    }
}

// ---------- авто-индексация (Phase-13) ----------
static INDEXING: AtomicBool = AtomicBool::new(false);
static INDEX_TICKS: AtomicU32 = AtomicU32::new(0);
const INDEX_EVERY_TICKS: u32 = 180; // фоновая переиндексация ~раз в 3 мин (таймер 1с)

// Запустить инкрементальную индексацию в фоновом потоке (не чаще одной зараз).
fn spawn_index(hwnd: HWND) {
    if INDEXING.swap(true, Ordering::SeqCst) {
        return; // уже идёт
    }
    let (chats_db, projects_root) = APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| (a.config.chats_db.clone(), a.config.projects_root.clone()))
            .unwrap_or_default()
    });
    if chats_db.is_empty() || projects_root.is_empty() {
        INDEXING.store(false, Ordering::SeqCst);
        return;
    }
    let hwnd_i = hwnd.0 as isize;
    std::thread::spawn(move || {
        let _ = index::ensure_index(&chats_db, &projects_root);
        INDEXING.store(false, Ordering::SeqCst);
        // свежая база -> обновить активный поиск
        unsafe {
            let _ = PostMessageW(HWND(hwnd_i as *mut core::ffi::c_void), WM_APP_SEARCH, WPARAM(0), LPARAM(0));
        }
    });
}

// ---------- индекс файлов history (Phase-13 Ф-C) ----------
static FILES_INDEXING: AtomicBool = AtomicBool::new(false);

// Построить/освежить индекс файлов в фоне (COM-резолв .lnk -> извлечение текста).
fn spawn_files_index(hwnd: HWND) {
    if FILES_INDEXING.swap(true, Ordering::SeqCst) {
        return;
    }
    let files_db = APP.with(|c| c.borrow().as_ref().map(|a| a.config.files_db.clone()).unwrap_or_default());
    if files_db.is_empty() {
        FILES_INDEXING.store(false, Ordering::SeqCst);
        return;
    }
    let hwnd_i = hwnd.0 as isize;
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let docs = collect_history_docs();
        let _ = index::ensure_files_index(&files_db, &docs);
        unsafe {
            CoUninitialize();
        }
        FILES_INDEXING.store(false, Ordering::SeqCst);
        unsafe {
            let _ = PostMessageW(HWND(hwnd_i as *mut core::ffi::c_void), WM_APP_SEARCH, WPARAM(0), LPARAM(0));
        }
    });
}

// Пути файлов из Windows Recent: резолвим цели всех .lnk, что реально файлы.
fn collect_history_docs() -> Vec<String> {
    let Some(appdata) = std::env::var_os("APPDATA") else {
        return Vec::new();
    };
    let dir = std::path::PathBuf::from(appdata).join("Microsoft").join("Windows").join("Recent");
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case("lnk")) != Some(true) {
                continue;
            }
            if let Some(target) = unsafe { resolve_lnk(&p) } {
                if std::path::Path::new(&target).is_file() {
                    out.push(target);
                }
            }
        }
    }
    out
}

// Резолв .lnk -> путь цели через IShellLinkW (вызывать в COM-инициализированном потоке).
unsafe fn resolve_lnk(lnk: &std::path::Path) -> Option<String> {
    let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
    let pf: IPersistFile = link.cast().ok()?;
    let wide: Vec<u16> = lnk.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    pf.Load(PCWSTR(wide.as_ptr()), STGM_READ).ok()?;
    let mut buf = [0u16; 260];
    let mut wfd = WIN32_FIND_DATAW::default();
    link.GetPath(&mut buf, &mut wfd, 0).ok()?;
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    (len > 0).then(|| String::from_utf16_lossy(&buf[..len]))
}

// ---------- пути документов для меню «ссылка/проводник» (Phase-14) ----------
// Карта basename(lowercase) -> полный путь файла из Windows Recent (.lnk резолв).
// Меню документа (Word/Excel/MS Project) читает её на UI-потоке; строится в фоне (COM).
static DOC_PATHS: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());
// Папки проектов из чат-индекса (D-02): фон делает DB-скан, ПКМ матчит по basename в памяти —
// чтобы не открывать SQLite синхронно на UI-потоке при каждом правом клике (лаг под пишущим индексатором).
static PROJ_FOLDERS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static DOC_PATHS_BUILDING: AtomicBool = AtomicBool::new(false);

// Построить/освежить в фоне карты для меню «ссылка/проводник»: пути документов (резолв .lnk Recent
// через COM) и папки проектов (DB-скан chats_db). Не морозит UI.
fn spawn_doc_paths() {
    if DOC_PATHS_BUILDING.swap(true, Ordering::SeqCst) {
        return;
    }
    let chats_db = APP.with(|c| c.borrow().as_ref().map(|a| a.config.chats_db.clone()).unwrap_or_default());
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        // ponytail: повторяет проход collect_history_docs с files-индексом; терпимо (фон, редко)
        let mut map = BTreeMap::new();
        for path in collect_history_docs() {
            if let Some(base) = std::path::Path::new(&path).file_name().and_then(|s| s.to_str()) {
                map.entry(base.to_lowercase()).or_insert(path); // первый матч при коллизии basename (risk-21)
            }
        }
        if let Ok(mut g) = DOC_PATHS.lock() {
            *g = map;
        }
        // папки проектов из чат-индекса (полный скан — здесь, в фоне, не на UI)
        let folders = search::project_folders(&chats_db);
        if let Ok(mut g) = PROJ_FOLDERS.lock() {
            *g = folders;
        }
        unsafe {
            CoUninitialize();
        }
        DOC_PATHS_BUILDING.store(false, Ordering::SeqCst);
    });
}

// Полный путь документа по имени окна (basename, регистронезависимо) или None.
fn doc_path_for(name: &str) -> Option<String> {
    let key = name.trim().to_lowercase();
    DOC_PATHS.lock().ok()?.get(&key).cloned()
}

// Папка проекта по имени окна — чистый матч по кэшу PROJ_FOLDERS (без БД, безопасно на UI).
fn proj_folder_for(name: &str) -> Option<String> {
    let folders = PROJ_FOLDERS.lock().ok()?;
    search::folder_for_project(&folders, name)
}

// Положить текст в буфер обмена (CF_UNICODETEXT). При успехе владение hmem уходит буферу.
unsafe fn copy_to_clipboard(hwnd: HWND, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let Ok(hmem) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) else {
        return;
    };
    let dst = GlobalLock(hmem);
    if dst.is_null() {
        return; // ponytail: редкий отказ lock -> терпимая утечка hmem
    }
    std::ptr::copy_nonoverlapping(wide.as_ptr(), dst as *mut u16, wide.len());
    let _ = GlobalUnlock(hmem);
    if OpenClipboard(hwnd).is_ok() {
        let _ = EmptyClipboard();
        let _ = SetClipboardData(CF_UNICODETEXT, HANDLE(hmem.0)); // успех -> hmem владеет буфер
        let _ = CloseClipboard();
    }
}

// Открыть Проводник с выделением файла: explorer.exe /select,"<path>".
unsafe fn open_in_explorer_select(path: &str) {
    let file: Vec<u16> = "explorer.exe".encode_utf16().chain(std::iter::once(0)).collect();
    let params: Vec<u16> = format!("/select,\"{}\"", path).encode_utf16().chain(std::iter::once(0)).collect();
    ShellExecuteW(None, w!("open"), PCWSTR(file.as_ptr()), PCWSTR(params.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL);
}

fn run_live_search(hwnd: HWND) {
    unsafe { hide_history_dropdown() }; // изменение текста закрывает историю
    let q = APP.with(|c| c.borrow().as_ref().map(|a| edit_text(a.search_edit)).unwrap_or_default());
    let q = q.trim().to_string();
    APP.with(|c| {
        if let Some(a) = c.borrow_mut().as_mut() {
            if q.chars().count() >= SEARCH_MIN {
                let files_db = a.config.search_files.then(|| a.config.files_db.clone());
                a.search_hits = search::search_bm25(&a.config.chats_db, files_db.as_deref(), &q, 200);
            } else {
                a.search_hits.clear();
            }
            rebuild_rows(a);
            unsafe { render::resize(hwnd, a) };
        }
    });
    unsafe {
        let _ = InvalidateRect(hwnd, None, BOOL(0));
    }
}

// Enter: записать запрос в историю + перезапросить BM25 (подхватит свежий авто-индекс).
fn commit_search_enter(hwnd: HWND) {
    let q = APP.with(|c| c.borrow().as_ref().map(|a| edit_text(a.search_edit)).unwrap_or_default());
    record_history(&q);
    run_live_search(hwnd);
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

// Цель пунктов «ссылка/проводник» по индексу окна: (путь, is_file). None -> пункты серые.
// Проект (Code/Cursor) -> папка проекта из chats_db; документ (Word/Excel/MS Project) -> файл из Recent; иначе None.
fn menu_link_for(a: &App, wi: usize) -> Option<(String, bool)> {
    let it = a.items.get(wi)?;
    match a.config.apps.get(it.app)?.mode {
        config::NameMode::Project { .. } => {
            proj_folder_for(&it.name).map(|f| (f, false)) // кэш PROJ_FOLDERS, без БД на UI (D-02)
        }
        config::NameMode::Document | config::NameMode::DocumentLast => {
            doc_path_for(&it.name).map(|p| (p, true))
        }
        config::NameMode::Whole => None,
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let menu = CreatePopupMenu().unwrap_or_default();
    // Phase-14: ссылка/проводник — в начале меню (до палитры); серые, если путь не резолвится
    let has_link = APP.with(|c| c.borrow().as_ref().map(|a| a.menu_link.is_some()).unwrap_or(false));
    let lflag = if has_link { MF_STRING } else { MF_STRING | MF_GRAYED };
    let _ = AppendMenuW(menu, lflag, ID_COPY_LINK, w!("Скопировать ссылку"));
    let _ = AppendMenuW(menu, lflag, ID_OPEN_DIR, w!("Открыть в проводнике"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
    for (i, p) in PALETTE.iter().enumerate() {
        let name: Vec<u16> = p.0.encode_utf16().chain(std::iter::once(0)).collect();
        let _ = AppendMenuW(menu, MF_STRING, ID_COLOR_BASE + i, PCWSTR(name.as_ptr()));
    }
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
    let _ = AppendMenuW(menu, MF_STRING, ID_LABEL, w!("Метка…"));
    let _ = AppendMenuW(menu, MF_STRING, ID_LABEL_CLEAR, w!("Убрать метку"));
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    // меню модальное: на время прячем тултип и глушим тики таймера (иначе меню закроется)
    hide_tooltip(hwnd);
    let _ = KillTimer(hwnd, ID_TIP_TIMER);
    MENU_ACTIVE.store(true, Ordering::Relaxed);
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(menu, TPM_LEFTALIGN | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
    MENU_ACTIVE.store(false, Ordering::Relaxed);
    let _ = DestroyMenu(menu);
}

// Меню настроек панели (вызывается кликом «⚙» в шапке): выбор шрифта и «О программе».
unsafe fn show_settings_menu(hwnd: HWND) {
    let menu = CreatePopupMenu().unwrap_or_default();
    let _ = AppendMenuW(menu, MF_STRING, ID_SET_FONT, w!("Шрифт…"));
    let files_on = APP.with(|c| c.borrow().as_ref().map(|a| a.config.search_files).unwrap_or(false));
    let fflag = if files_on { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(menu, fflag, ID_TOGGLE_FILES, w!("Искать в файлах (history)"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
    let _ = AppendMenuW(menu, MF_STRING, ID_ABOUT, w!("О программе…"));
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    // меню модальное: на время прячем тултип и глушим тики таймера (иначе меню закроется)
    hide_tooltip(hwnd);
    let _ = KillTimer(hwnd, ID_TIP_TIMER);
    MENU_ACTIVE.store(true, Ordering::Relaxed);
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(menu, TPM_LEFTALIGN | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
    MENU_ACTIVE.store(false, Ordering::Relaxed);
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
    // настройки: переключить scope «+Файлы» (history)
    if id == ID_TOGGLE_FILES {
        let on = APP.with(|c| {
            c.borrow_mut()
                .as_mut()
                .map(|a| {
                    a.config.search_files = !a.config.search_files;
                    a.config.save(hwnd); // персист scope
                    a.config.search_files
                })
                .unwrap_or(false)
        });
        if on {
            spawn_files_index(hwnd); // построить индекс файлов в фоне
            unsafe { set_search_cue(w!("⏳ Индексирую файлы…")) };
        }
        run_live_search(hwnd); // пересчитать выдачу с учётом нового scope
        return;
    }
    // Phase-14: «Скопировать ссылку» / «Открыть в проводнике» — берут готовый a.menu_link
    if id == ID_COPY_LINK || id == ID_OPEN_DIR {
        let link = APP.with(|c| c.borrow().as_ref().and_then(|a| a.menu_link.clone()));
        let Some((path, is_file)) = link else { return };
        unsafe {
            if id == ID_COPY_LINK {
                copy_to_clipboard(hwnd, &path);
            } else if is_file {
                open_in_explorer_select(&path); // документ: Explorer с выделением файла
            } else {
                let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
                ShellExecuteW(None, w!("open"), PCWSTR(wide.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
            }
        }
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
            menu_link: None,
            last_h: 0,
            bell: HashSet::new(),
            search_hits: Vec::new(),
            search_edit: HWND(std::ptr::null_mut()),
            tooltip: HWND(std::ptr::null_mut()),
            tip_row: -1,
            search_history: load_history(),
            hist_list: HWND(std::ptr::null_mut()),
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
        create_search_box(hwnd);
        create_tooltip(hwnd);
        spawn_index(hwnd); // первичная индексация чатов в фоне
        spawn_doc_paths(); // карта путей документов для меню «ссылка/проводник» (Phase-14)
        let files_on = APP.with(|c| c.borrow().as_ref().map(|a| a.config.search_files).unwrap_or(false));
        if files_on {
            spawn_files_index(hwnd); // scope «+Файлы» сохранён -> построить индекс файлов
        }
        SetTimer(hwnd, 1, 1000, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}
