#![windows_subsystem = "windows"]
//! ClaudeBar — крошечная всегда-поверх панель для переключения между открытыми
//! окнами редакторов (VS Code / Cursor), в которых крутится Claude Code.
//! ЛКМ по строке — перейти в окно. ПКМ — задать цвет и метку. Привязка по имени проекта.

// FILE: src/main.rs
// VERSION: 1.0.0
// START_MODULE_CONTRACT
//   PURPOSE: Точка входа Win32 + оконная процедура + состояние App + оркестрация всех модулей панели.
//   SCOPE: создание окна (always-on-top, tool-window), цикл сообщений, диспетчеризация WndProc; App (thread_local APP);
//          опрос (окна/сигналы/недавние) по таймеру, анимационный таймер, глобальный хоткей; шапка/поиск/история;
//          контекст-меню, tooltip, буфер обмена/Explorer; оркестрация голоса (M-VOICE) и вставки (M-PASTE).
//   DEPENDS: M-CONFIG, M-WINENUM, M-ACTIVATE, M-RENDER, M-RECENT, M-ICON, M-SIGNAL, M-SETTINGS, M-SEARCH, M-INDEX, M-SDAEMON, M-VOICE, M-AUDIO, M-STT, M-TRANSFORM, M-PASTE, M-PROMPT
//   LINKS: M-MAIN
//   ROLE: ENTRY_POINT
//   MAP_MODE: SUMMARY
//   NOTE: Блочная разметка (START_BLOCK) отложена до рефактора «App -> state-модуль» (FPF D-5/D-23); контракт добавлен ретроспективно.
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   App / APP            - состояние панели (thread_local RefCell); поля окон/недавних/поиска/сигналов/голоса
//   wndproc              - оконная процедура: paint, клики/hover, таймеры, меню, WM_APP_* (поиск/голос)
//   refresh_items        - опрос: окна (M-WINENUM), недавние (M-RECENT), сигналы bell/busy + sessions (M-SIGNAL)
//   rebuild_rows         - пересборка строк (M-RENDER.build_rows) + фильтр недавних по поиску + блок «Найдено ещё»
//   run_live_search / commit_search_enter - живой BM25 (M-SEARCH), история запросов
//   handle_menu_command  - контекст-меню: цвет/метка/сортировка/⚙-настройки (шрифт, полные пути, голос-опции)
//   update_anim_timer    - аним-таймер (змейка квадратов / полоса голоса) только при активности
//   hotkey / voice       - глобальный хоткей -> voice.toggle; WM_APP_VOICE_DONE -> take_result + paste_text
//   url_to_path / percent_decode / explorer_folder_path - «ссылка/проводник» (COM, побайтный percent-decode)
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.0.0 - FPF D-23: добавлен MODULE_CONTRACT/MODULE_MAP (main.rs был единственным модулем без разметки). Контракт ретроспективный (SUMMARY); полная блочная разметка — в рефакторе App->state.
// END_CHANGE_SUMMARY

mod activate;
mod audio;
mod config;
mod paste;
mod icon;
mod index;
mod prompt;
mod recent;
mod render;
#[allow(dead_code)] // dormant: dense отложен (Phase-13); M-SDAEMON оживёт с Python-модулем смысла
mod sdaemon;
#[allow(dead_code)] // dense-помощники dormant + snippet_for ждёт Ф-B (тултипы)
mod search;
mod searchbox;
mod settings;
mod signal;
mod state;
mod stt;
mod tooltip;
mod transform;
mod voice;
mod win_enum;

// App/APP живут в M-STATE; ре-экспорт, чтобы crate::App работал для M-RENDER и прочих (рефактор god-object).
pub(crate) use state::{App, APP};
// tooltip-функции вызываются из wndproc/subclass под прежними именами.
use tooltip::{arm_tip, create_tooltip, hide_tooltip, show_tooltip};
// поле поиска и история — M-SEARCHBOX; wndproc зовёт под прежними именами.
use searchbox::{
    create_search_box, hide_history_dropdown, load_history, record_history, set_search_cue, SEARCH_MIN,
};

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use config::{Config, PALETTE};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, ReleaseCapture, SetCapture, TrackMouseEvent, UnregisterHotKey, HOT_KEY_MODIFIERS,
    MOD_NOREPEAT, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_ALL, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::UI::Shell::{
    IShellLinkW, IShellWindows, IWebBrowserApp, ShellExecuteW, ShellLink, ShellWindows,
};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::WindowsAndMessaging::*;
use std::os::windows::ffi::OsStrExt;

pub(crate) const WM_MOUSELEAVE: u32 = 0x02A3; // делится с субклассом поля поиска (M-SEARCHBOX)
pub(crate) const ID_TIP_TIMER: usize = 3; // dwell-таймер подсказки (~0.5с) — M-TOOLTIP + wndproc
pub(crate) const TIP_DELAY: u32 = 500; // мс выдержки перед показом подсказки — M-TOOLTIP
const ID_ANIM_TIMER: usize = 4; // таймер анимации бегущих точек busy (Phase-17)
const ANIM_MS: u32 = 350; // мс между кадрами анимации точек
const HOTKEY_VOICE: i32 = 1; // id глобального хоткея диктовки (RegisterHotKey) — Phase-19
pub(crate) const TIP_SEARCHBOX: i32 = -2; // tip_row: курсор над строкой поиска -> правила — M-TOOLTIP + wndproc

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
const ID_FULLPATHS: usize = 33; // меню настроек: включить полные пути в заголовках редакторов (Phase-15)
const ID_SORT: usize = 34; // меню настроек: сортировка окон по времени (recent) vs по имени (alpha) — Phase-16
const ID_KEEP_CLIP: usize = 35; // меню настроек: сохранять прежний буфер обмена после диктовки — Phase-20
const ID_MIC_ALWAYS: usize = 36; // меню настроек: микрофон всегда включён (always-on + pre-roll) — Phase-22
const ID_TRAIL_SPACE: usize = 37; // меню настроек: пробел после надиктованной фразы — Phase-23
const ID_STREAMING: usize = 38; // меню настроек: стриминг длинной диктовки — Phase-24
const WM_APP_SEARCH: u32 = WM_APP + 1; // dense-результаты из фонового потока

// ---------- состояние ----------
// App / APP вынесены в M-STATE (src/state.rs); здесь доступны через ре-экспорт выше.
// Поле поиска, история, id ID_SEARCH/ID_HIST_LIST и LB_*/EM_*-константы — в M-SEARCHBOX (src/searchbox.rs).

// ---------- перечисление окон ----------
// Порядок первого появления окна за сессию (для sort=recent и «новое -> вниз») — Phase-16.
static ORDINALS: Mutex<BTreeMap<isize, u64>> = Mutex::new(BTreeMap::new());
static NEXT_ORDINAL: AtomicU64 = AtomicU64::new(1);

fn window_ordinal(hwnd: HWND) -> u64 {
    let key = hwnd.0 as isize;
    let Ok(mut m) = ORDINALS.lock() else { return 0 };
    if let Some(&o) = m.get(&key) {
        return o;
    }
    // ponytail: закрытые hwnd остаются в карте (рост за сессию), не чистим — мелочь
    let o = NEXT_ORDINAL.fetch_add(1, Ordering::Relaxed);
    m.insert(key, o);
    o
}

// Возвращает true, если присвоены новые № путей (нужно сохранить реестр).
fn refresh_items(app: &mut App) -> bool {
    let raw = win_enum::list_windows();
    app.items = win_enum::match_windows(&raw, &app.config.apps);
    // порядковый № первого появления окна (sort=recent, «новое -> вниз»)
    for it in app.items.iter_mut() {
        it.ordinal = window_ordinal(it.hwnd);
    }
    // присвоить стабильные № новым полным путям (для показа дублей «(N)»)
    let mut numbered = false;
    let paths: Vec<String> = app.items.iter().filter_map(|it| it.path.clone()).collect();
    for p in paths {
        if app.config.number_for(&p).is_none() {
            app.config.assign_number(&p);
            numbered = true;
        }
    }
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
    app.bell_paths = signal::bell_cwds(); // точная подсветка по полному пути (Phase-15)
    app.busy = signal::busy_keys();
    app.busy_paths = signal::busy_cwds(); // бегущие точки «идёт работа» (Phase-17)
    app.sessions = signal::sessions(); // Phase-26 presence: агент + состояние (idle/working/done) на квадрат
    numbered
}

// Включить/выключить таймер анимации точек: идёт только пока есть busy (в простое не мигаем) — Phase-17.
fn update_anim_timer(hwnd: HWND, app: &App) {
    let active = !app.busy.is_empty()
        || !app.busy_paths.is_empty()
        || app.sessions.iter().any(|s| s.state == signal::SessState::Working) // бегущая змейка «работает» (Phase-26)
        || app.voice.state() != voice::VoiceState::Idle; // анимация полосы при записи/распознавании — Phase-19
    unsafe {
        if active {
            let _ = SetTimer(hwnd, ID_ANIM_TIMER, ANIM_MS, None);
        } else {
            let _ = KillTimer(hwnd, ID_ANIM_TIMER);
        }
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
                if wp.0 == ID_ANIM_TIMER {
                    // кадр анимации индикатора + авто-стоп записи по тишине (Phase-17/19)
                    APP.with(|c| {
                        if let Some(a) = c.borrow_mut().as_mut() {
                            a.anim_frame = a.anim_frame.wrapping_add(1);
                            a.voice.stream_tick(hwnd, &a.config); // стрим-заход (Phase-24, троттлинг/гейт внутри)
                            if a.voice.poll(hwnd, &a.config) {
                                // тишина -> авто-стоп: пересчитать таймер и высоту окна
                                update_anim_timer(hwnd, a);
                                render::resize(hwnd, a);
                            }
                        }
                    });
                    let _ = InvalidateRect(hwnd, None, BOOL(0));
                    return LRESULT(0);
                }
                if wp.0 == ID_TIP_TIMER {
                    let _ = KillTimer(hwnd, ID_TIP_TIMER);
                    show_tooltip(hwnd);
                    return LRESULT(0);
                }
                APP.with(|c| {
                    if let Some(app) = c.borrow_mut().as_mut() {
                        if refresh_items(app) {
                            app.config.save(hwnd); // новые № путей -> сохранить реестр
                        }
                        update_anim_timer(hwnd, app); // вкл/выкл анимацию точек по наличию busy
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
                    LinkOnly(String, bool), // путь, is_file — мини-меню копирования для недавних/закрытых строк
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
                        // недавний/закрытый документ -> мини-меню «скопировать путь / открыть в проводнике»
                        render::Row::Recent { ridx } => a
                            .recent
                            .get(ridx)
                            .map(|d| R::LinkOnly(recent_path(d), matches!(d.open, recent::OpenCmd::Lnk(_)))),
                        // закрытая папка-совпадение поиска («Найдено ещё»)
                        render::Row::SearchResult { hit } => {
                            a.search_hits.get(hit).map(|h| R::LinkOnly(h.folder.clone(), false))
                        }
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
                        show_menu(hwnd, false);
                    }
                    Some(R::LinkOnly(path, is_file)) => {
                        APP.with(|c| {
                            if let Some(a) = c.borrow_mut().as_mut() {
                                a.menu_link = Some((path, is_file));
                            }
                        });
                        show_menu(hwnd, true); // мини-меню: только копировать путь / открыть в проводнике
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
                // поле поиска / список истории обслуживает M-SEARCHBOX; остальное — команды меню
                if !searchbox::on_command(hwnd, id, notif) {
                    handle_command(hwnd, id);
                }
                LRESULT(0)
            }
            WM_DISPLAYCHANGE => {
                // Конфигурация мониторов сменилась на ходу: если окно осталось за
                // пределами видимых экранов — вернуть его на видимый, иначе его не
                // схватить мышью (WS_EX_TOOLWINDOW: нет в Alt+Tab/таскбаре).
                let mut rc = RECT::default();
                if GetWindowRect(hwnd, &mut rc).is_ok() {
                    let default_pos = (GetSystemMetrics(SM_CXSCREEN) - render::W - 20, 40);
                    let (vx, vy, vw, vh) = (
                        GetSystemMetrics(SM_XVIRTUALSCREEN),
                        GetSystemMetrics(SM_YVIRTUALSCREEN),
                        GetSystemMetrics(SM_CXVIRTUALSCREEN),
                        GetSystemMetrics(SM_CYVIRTUALSCREEN),
                    );
                    if let Some((x, y)) = config::recover_pos(
                        (rc.left, rc.top),
                        default_pos,
                        render::W,
                        rc.bottom - rc.top,
                        vx,
                        vy,
                        vw,
                        vh,
                    ) {
                        let _ = SetWindowPos(
                            hwnd,
                            HWND_TOPMOST,
                            x,
                            y,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOACTIVATE,
                        );
                        APP.with(|c| {
                            if let Some(app) = c.borrow_mut().as_mut() {
                                app.config.pos = Some((x, y));
                                app.config.save_pos();
                            }
                        });
                    }
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                let _ = UnregisterHotKey(hwnd, HOTKEY_VOICE); // снять глобальный хоткей диктовки — Phase-19
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
                // фон/текст поля поиска — светло-голубой (M-SEARCHBOX), чтобы белый не резал на тёмном
                LRESULT(searchbox::ctl_color_edit(HDC(wp.0 as *mut core::ffi::c_void)))
            }
            m if m == WM_APP_SEARCH => {
                // индекс готов -> вернуть обычную подсказку и перезапросить поиск
                set_search_cue(w!("Поиск по чатам…"));
                run_live_search(hwnd);
                LRESULT(0)
            }
            WM_HOTKEY if wp.0 as i32 == HOTKEY_VOICE => {
                // голосовой ввод (Phase-19): toggle записи/распознавания
                voice::vlog("WM_HOTKEY: голосовой хоткей пойман");
                APP.with(|c| {
                    if let Some(a) = c.borrow_mut().as_mut() {
                        // на старте записи запомнить окно-получатель вставки (не нашу панель)
                        if a.voice.state() == voice::VoiceState::Idle {
                            let fg = GetForegroundWindow();
                            if fg != hwnd {
                                a.voice_target = fg;
                            }
                        }
                        a.voice.toggle(hwnd, &a.config);
                        update_anim_timer(hwnd, a);
                        render::resize(hwnd, a); // вырастить/сжать окно под баннер
                    }
                });
                let _ = InvalidateRect(hwnd, None, FALSE);
                LRESULT(0)
            }
            m if m == voice::WM_APP_STREAM_PARTIAL => {
                // частичное распознавание стриминга: зафиксировать устоявшиеся сегменты (Phase-24).
                // Вставки нет — только накопление committed_text/сдвиг окна внутри Voice.
                let id = lp.0 as u64;
                APP.with(|c| {
                    if let Some(a) = c.borrow_mut().as_mut() {
                        a.voice.on_partial(id);
                    }
                });
                LRESULT(0)
            }
            m if m == voice::WM_APP_VOICE_DONE => {
                // распознавание готово: lparam = id результата в реестре (НЕ сырой указатель);
                // чужое сообщение с мусорным id -> None -> пустая строка, без UB (audit #3)
                let text = voice::take_result(lp.0 as u64).unwrap_or_default();
                let target = APP.with(|c| {
                    c.borrow_mut()
                        .as_mut()
                        .map(|a| {
                            a.voice.on_done();
                            a.voice_target
                        })
                        .unwrap_or(HWND(std::ptr::null_mut()))
                });
                voice::vlog(&format!(
                    "WM_APP_VOICE_DONE: текст {} симв, target={:?}",
                    text.chars().count(),
                    target.0
                ));
                voice::cue_end(); // звук конца распознавания
                if !text.is_empty() {
                    // вернуть фокус в исходное поле, если он увёлся, и вставить
                    if !target.0.is_null() && GetForegroundWindow() != target {
                        activate::activate(target);
                    }
                    let keep = APP.with(|c| {
                        c.borrow().as_ref().map(|a| a.config.voice_keep_clipboard).unwrap_or(true)
                    });
                    let ok = paste::paste_text(&text, keep);
                    voice::vlog(&format!("paste_text -> {ok} (keep_clip={keep})"));
                }
                APP.with(|c| {
                    if let Some(a) = c.borrow_mut().as_mut() {
                        update_anim_timer(hwnd, a);
                        render::resize(hwnd, a); // сжать окно (баннер скрыт в Idle)
                    }
                });
                let _ = InvalidateRect(hwnd, None, FALSE);
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
pub(crate) fn edit_text(edit: HWND) -> String {
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
    let q = edit_text(app.search_edit); // текст поиска: недавние фильтруются по имени (build_rows)
    app.rows = render::build_rows(&app.items, &app.recent, &app.config.apps, &app.config, q.trim());
    if !app.search_hits.is_empty() {
        let open: HashSet<String> = app.items.iter().map(|it| it.name.to_lowercase()).collect();
        app.rows.extend(render::search_result_rows(&app.search_hits, &open));
    }
}

// ---------- поле поиска + история вынесены в M-SEARCHBOX (src/searchbox.rs) ----------
// ---------- подсказки (tooltip) вынесены в M-TOOLTIP (src/tooltip.rs) ----------

pub(crate) fn recent_path(d: &recent::RecentDoc) -> String {
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
        let _ = GlobalFree(hmem); // владение буферу не передано -> освобождаем (D-03)
        return;
    }
    std::ptr::copy_nonoverlapping(wide.as_ptr(), dst as *mut u16, wide.len());
    let _ = GlobalUnlock(hmem);
    if OpenClipboard(hwnd).is_ok() {
        let _ = EmptyClipboard();
        // владение hmem уходит буферу ТОЛЬКО при успешном SetClipboardData; иначе освобождаем (D-03)
        if SetClipboardData(CF_UNICODETEXT, HANDLE(hmem.0)).is_err() {
            let _ = GlobalFree(hmem);
        }
        let _ = CloseClipboard();
    } else {
        let _ = GlobalFree(hmem); // буфер обмена не открылся -> hmem ничей (D-03)
    }
}

// Открыть Проводник с выделением файла: explorer.exe /select,"<path>".
unsafe fn open_in_explorer_select(path: &str) {
    let file: Vec<u16> = "explorer.exe".encode_utf16().chain(std::iter::once(0)).collect();
    let params: Vec<u16> = format!("/select,\"{}\"", path).encode_utf16().chain(std::iter::once(0)).collect();
    ShellExecuteW(None, w!("open"), PCWSTR(file.as_ptr()), PCWSTR(params.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL);
}

pub(crate) fn run_live_search(hwnd: HWND) {
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
pub(crate) fn commit_search_enter(hwnd: HWND) {
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
            // путь окна (из заголовка) надёжнее; иначе кэш PROJ_FOLDERS (Phase-15)
            it.path.clone().or_else(|| proj_folder_for(&it.name)).map(|f| (f, false))
        }
        config::NameMode::Document | config::NameMode::DocumentLast => {
            doc_path_for(&it.name).map(|p| (p, true))
        }
        // Проводник (и т.п. «целый заголовок»): заголовок даёт лишь имя папки —
        // реальный путь открытой папки берём через Shell COM по hwnd. Терминалы -> None.
        config::NameMode::Whole => unsafe { explorer_folder_path(it.hwnd).map(|f| (f, false)) },
    }
}

// file:///D:/Python/claudebar -> D:\Python\claudebar ; file://server/share -> \\server\share
fn url_to_path(url: &str) -> Option<String> {
    if let Some(rest) = url.strip_prefix("file:///") {
        let p = percent_decode(rest).replace('/', "\\");
        return (!p.is_empty()).then_some(p);
    }
    if let Some(rest) = url.strip_prefix("file://") {
        let p = percent_decode(rest).replace('/', "\\");
        return (!p.is_empty()).then(|| format!("\\\\{p}"));
    }
    None
}

// Два ASCII-hex байта -> байт; не-hex -> None. Побайтно, БЕЗ среза &str (иначе паника
// "not a char boundary" на '%' перед многобайтовым символом). ponytail: дубль recent::hex2 — свести в util при рефакторе.
fn hex2(hi: u8, lo: u8) -> Option<u8> {
    let d = |c: u8| (c as char).to_digit(16);
    Some((d(hi)? * 16 + d(lo)?) as u8)
}

// Декодировать %XX в URL (пробелы, кириллица и пр.). Прочее — как есть.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            if let Some(v) = hex2(b[i + 1], b[i + 2]) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// Путь открытой папки окна Проводника через Shell COM (IShellWindows -> IWebBrowserApp по hwnd).
unsafe fn explorer_folder_path(target: HWND) -> Option<String> {
    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED); // UI-поток; баланс CoUninitialize ниже
    let result = (|| {
        let shell: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL).ok()?;
        let count = shell.Count().ok()?;
        for i in 0..count {
            let Ok(disp) = shell.Item(&VARIANT::from(i)) else { continue };
            let Ok(wb) = disp.cast::<IWebBrowserApp>() else { continue };
            if wb.HWND().unwrap_or_default().0 != target.0 as isize {
                continue;
            }
            let url = wb.LocationURL().ok()?;
            return url_to_path(&url.to_string());
        }
        None
    })();
    CoUninitialize();
    result
}

// minimal=true (недавние/закрытые/результаты поиска): только «Скопировать ссылку»/«Открыть в проводнике»,
// без палитры цветов и меток (они привязаны к проекту открытого окна).
unsafe fn show_menu(hwnd: HWND, minimal: bool) {
    let menu = CreatePopupMenu().unwrap_or_default();
    // Phase-14: ссылка/проводник — в начале меню (до палитры); серые, если путь не резолвится
    let has_link = APP.with(|c| c.borrow().as_ref().map(|a| a.menu_link.is_some()).unwrap_or(false));
    let lflag = if has_link { MF_STRING } else { MF_STRING | MF_GRAYED };
    let _ = AppendMenuW(menu, lflag, ID_COPY_LINK, w!("Скопировать ссылку"));
    let _ = AppendMenuW(menu, lflag, ID_OPEN_DIR, w!("Открыть в проводнике"));
    if !minimal {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        for (i, p) in PALETTE.iter().enumerate() {
            let name: Vec<u16> = p.0.encode_utf16().chain(std::iter::once(0)).collect();
            let _ = AppendMenuW(menu, MF_STRING, ID_COLOR_BASE + i, PCWSTR(name.as_ptr()));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_LABEL, w!("Метка…"));
        let _ = AppendMenuW(menu, MF_STRING, ID_LABEL_CLEAR, w!("Убрать метку"));
    }
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
    let _ = AppendMenuW(menu, MF_STRING, ID_FULLPATHS, w!("Полные пути в заголовках редакторов"));
    let recent_on = APP.with(|c| c.borrow().as_ref().map(|a| a.config.is_sort_recent()).unwrap_or(false));
    let sflag = if recent_on { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(menu, sflag, ID_SORT, w!("Сортировка по времени (новые внизу)"));
    let keep_on = APP.with(|c| c.borrow().as_ref().map(|a| a.config.voice_keep_clipboard).unwrap_or(true));
    let kflag = if keep_on { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(menu, kflag, ID_KEEP_CLIP, w!("Сохранять прежний буфер обмена (диктовка)"));
    let mic_on = APP.with(|c| c.borrow().as_ref().map(|a| a.config.voice_always_on).unwrap_or(false));
    let mflag = if mic_on { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(menu, mflag, ID_MIC_ALWAYS, w!("Микрофон всегда включён (pre-roll)"));
    let trail_on = APP.with(|c| c.borrow().as_ref().map(|a| a.config.voice_trailing_space).unwrap_or(true));
    let tflag = if trail_on { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(menu, tflag, ID_TRAIL_SPACE, w!("Пробел после фразы (диктовка)"));
    let stream_on = APP.with(|c| c.borrow().as_ref().map(|a| a.config.voice_streaming).unwrap_or(false));
    let sflag = if stream_on { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(menu, sflag, ID_STREAMING, w!("Стриминг длинной диктовки"));
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
    // настройки: включить полные пути в заголовках редакторов (window.title с ${rootPath})
    if id == ID_FULLPATHS {
        let res = settings::configure_editor_titles();
        let msg = if res.is_empty() {
            "Не найден %APPDATA% — настроить не удалось.".to_string()
        } else {
            let lines: Vec<String> = res.iter().map(|(e, s)| format!("• {}: {}", e, s)).collect();
            format!(
                "Заголовки редакторов:\n{}\n\nПерезапусти редактор, чтобы применилось. Бэкап: settings.json.claudebar-bak",
                lines.join("\n")
            )
        };
        let wmsg: Vec<u16> = msg.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            MessageBoxW(hwnd, PCWSTR(wmsg.as_ptr()), w!("Полные пути"), MB_OK | MB_ICONINFORMATION);
        }
        return;
    }
    // настройки: переключить сортировку окон (alpha <-> recent)
    if id == ID_SORT {
        APP.with(|c| {
            if let Some(a) = c.borrow_mut().as_mut() {
                let v = !a.config.is_sort_recent();
                a.config.set_sort_recent(v);
                a.config.save(hwnd);
                rebuild_rows(a);
            }
        });
        unsafe {
            let _ = InvalidateRect(hwnd, None, BOOL(0));
        }
        return;
    }
    // настройки: сохранять ли прежний буфер обмена после вставки диктовки — Phase-20
    if id == ID_KEEP_CLIP {
        APP.with(|c| {
            if let Some(a) = c.borrow_mut().as_mut() {
                a.config.voice_keep_clipboard = !a.config.voice_keep_clipboard;
                a.config.save(hwnd); // персист флага
            }
        });
        return;
    }
    // настройки: always-on микрофон (always-on + pre-roll) — Phase-22
    if id == ID_MIC_ALWAYS {
        APP.with(|c| {
            if let Some(a) = c.borrow_mut().as_mut() {
                // D-14: применить сначала; конфиг/галочку менять ТОЛЬКО при успехе (на Idle),
                // иначе тоггл во время записи разъедет ini и реальность.
                let want = !a.config.voice_always_on;
                if a.voice.set_always_on(want) {
                    a.config.voice_always_on = want;
                    a.config.save(hwnd); // персист флага
                } else {
                    voice::vlog("ID_MIC_ALWAYS: игнор — идёт запись (конфиг не тронут)");
                }
            }
        });
        return;
    }
    // настройки: пробел после надиктованной фразы (диктовка не липнет к точке) — Phase-23
    if id == ID_TRAIL_SPACE {
        APP.with(|c| {
            if let Some(a) = c.borrow_mut().as_mut() {
                a.config.voice_trailing_space = !a.config.voice_trailing_space;
                a.config.save(hwnd); // персист флага
            }
        });
        return;
    }
    // настройки: стриминг длинной диктовки (Phase-24)
    if id == ID_STREAMING {
        APP.with(|c| {
            if let Some(a) = c.borrow_mut().as_mut() {
                a.config.voice_streaming = !a.config.voice_streaming;
                a.config.save(hwnd); // персист флага
            }
        });
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
        if let Some(s) = prompt::prompt_text(hwnd, hinst, &cur) {
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
    // диагностика: при panic (panic=abort) записать причину в voice.log перед падением
    std::panic::set_hook(Box::new(|info| {
        voice::vlog(&format!("PANIC: {info}"));
    }));
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
        let voice_hotkey = config.voice_hotkey.clone(); // для RegisterHotKey после создания окна — Phase-19

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
            bell_paths: HashSet::new(),
            busy: HashSet::new(),
            busy_paths: HashSet::new(),
            sessions: Vec::new(),
            anim_frame: 0,
            search_hits: Vec::new(),
            search_edit: HWND(std::ptr::null_mut()),
            tooltip: HWND(std::ptr::null_mut()),
            tip_row: -1,
            search_history: load_history(),
            hist_list: HWND(std::ptr::null_mut()),
            reorder: false,
            drag: None,
            voice: voice::Voice::new(),
            voice_target: HWND(std::ptr::null_mut()),
        };
        if refresh_items(&mut app) {
            app.config.save_pos(); // окна ещё нет -> сохраняем с известной позицией
        }
        // Phase-22: при включённой опции поднять always-on микрофон сразу на старте,
        // чтобы первое слово ловилось без cold-start (поток уже тёплый к первому хоткею).
        if app.config.voice_always_on {
            app.voice.set_always_on(true);
        }

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
        let h = render::HEAD + render::ROW * n + render::strip_h(app.voice.state());
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

        // глобальный хоткей диктовки (Phase-19): ловится из любого окна, не только когда панель в фокусе
        match config::parse_hotkey(&voice_hotkey) {
            Some((mods, vk)) => {
                let ok = RegisterHotKey(hwnd, HOTKEY_VOICE, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, vk).is_ok();
                voice::vlog(&format!(
                    "RegisterHotKey '{voice_hotkey}' (mods={mods:#x} vk={vk:#x}) -> {}",
                    if ok { "OK" } else { "ЗАНЯТ/ОШИБКА (хоткей не сработает)" }
                ));
            }
            None => voice::vlog(&format!("parse_hotkey не разобрал комбинацию: '{voice_hotkey}'")),
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{percent_decode, url_to_path};

    #[test]
    fn url_to_path_local_and_unc() {
        assert_eq!(url_to_path("file:///D:/Python/claudebar").as_deref(), Some("D:\\Python\\claudebar"));
        // %20 -> пробел, кириллица %D0.. декодируется
        assert_eq!(url_to_path("file:///C:/My%20Docs").as_deref(), Some("C:\\My Docs"));
        // UNC
        assert_eq!(url_to_path("file://server/share").as_deref(), Some("\\\\server\\share"));
        // не file:// -> None
        assert_eq!(url_to_path("http://x/y"), None);
    }

    #[test]
    fn percent_decode_basic() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("no-escapes"), "no-escapes");
        assert_eq!(percent_decode("%41%42"), "AB");
        // незавершённый % — как есть
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn percent_decode_no_panic_on_multibyte() {
        // regression: '%' перед 3-байтовым символом паниковал на срезе &s[i+1..i+3]; теперь % как есть
        assert_eq!(percent_decode("10%你"), "10%你");
        assert_eq!(percent_decode("c%3A/x"), "c:/x");
    }
}
