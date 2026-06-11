#![windows_subsystem = "windows"]
//! ClaudeBar — крошечная всегда-поверх панель для переключения между открытыми
//! окнами редакторов (VS Code / Cursor), в которых крутится Claude Code.
//! ЛКМ по строке — перейти в окно. ПКМ — задать цвет и метку. Привязка по имени проекта.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, ReleaseCapture, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

const EM_SETSEL: u32 = 0x00B1;

// ---------- геометрия и цвета ----------
const W: i32 = 252;
const HEAD: i32 = 24;
const ROW: i32 = 30;
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

// палитра цветов проектов: (имя, r, g, b)
const PALETTE: [(&str, u8, u8, u8); 8] = [
    ("Синий", 0x5B, 0x8F, 0xF9),
    ("Зелёный", 0x61, 0xD4, 0xA6),
    ("Жёлтый", 0xF6, 0xBD, 0x16),
    ("Красный", 0xE8, 0x68, 0x4A),
    ("Фиолетовый", 0xB3, 0x7F, 0xEB),
    ("Голубой", 0x6D, 0xC8, 0xEC),
    ("Розовый", 0xFF, 0x99, 0xC3),
    ("Серый", 0x9A, 0xA7, 0xB1),
];

// id команд меню
const ID_COLOR_BASE: usize = 1; // 1..=8
const ID_LABEL: usize = 20;
const ID_LABEL_CLEAR: usize = 21;

// ---------- состояние ----------
struct Item {
    hwnd: HWND,
    project: String,
}
#[derive(Clone, Default)]
struct Conf {
    color: i32, // -1 = авто по имени
    label: String,
}
struct App {
    hinst: HINSTANCE,
    items: Vec<Item>,
    conf: HashMap<String, Conf>,
    patterns: Vec<String>,
    cfg_path: PathBuf,
    font_main: HFONT,
    font_small: HFONT,
    hover: i32,
    menu_target: usize, // индекс строки, по которой открыли меню
    last_h: i32,
}

thread_local! {
    static APP: RefCell<Option<App>> = RefCell::new(None);
}

fn auto_color(name: &str) -> usize {
    let h = name
        .bytes()
        .fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    (h % PALETTE.len() as u32) as usize
}

impl App {
    fn color_idx(&self, project: &str) -> usize {
        match self.conf.get(project) {
            Some(c) if c.color >= 0 => (c.color as usize).min(PALETTE.len() - 1),
            _ => auto_color(project),
        }
    }
    fn label(&self, project: &str) -> String {
        self.conf.get(project).map(|c| c.label.clone()).unwrap_or_default()
    }
    fn set_color(&mut self, project: &str, idx: usize) {
        self.conf.entry(project.to_string()).or_default().color = idx as i32;
    }
    fn set_label(&mut self, project: &str, label: String) {
        self.conf.entry(project.to_string()).or_default().label = label;
    }
}

// ---------- конфиг ----------
fn default_patterns() -> Vec<String> {
    vec![
        " - Visual Studio Code".to_string(),
        " - Cursor".to_string(),
    ]
}

fn load_config(path: &PathBuf) -> (Vec<String>, HashMap<String, Conf>, Option<(i32, i32)>) {
    let mut patterns: Vec<String> = Vec::new();
    let mut conf: HashMap<String, Conf> = HashMap::new();
    let mut pos: Option<(i32, i32)> = None;
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("pos=") {
                let mut it = v.split(',');
                if let (Some(x), Some(y)) = (it.next(), it.next()) {
                    if let (Ok(x), Ok(y)) = (x.trim().parse(), y.trim().parse()) {
                        pos = Some((x, y));
                    }
                }
            } else if let Some(v) = line.strip_prefix("pattern=") {
                if !v.is_empty() {
                    patterns.push(v.to_string());
                }
            } else if let Some(v) = line.strip_prefix("p=") {
                let parts: Vec<&str> = v.splitn(3, '\t').collect();
                if parts.len() >= 2 {
                    let project = parts[0].to_string();
                    let color = parts[1].trim().parse::<i32>().unwrap_or(-1);
                    let label = parts.get(2).map(|s| s.to_string()).unwrap_or_default();
                    conf.insert(project, Conf { color, label });
                }
            }
        }
    }
    if patterns.is_empty() {
        patterns = default_patterns();
    }
    (patterns, conf, pos)
}

fn save_config(app: &App, hwnd: HWND) {
    let mut out = String::from("# claudebar config\n");
    let mut rc = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rc) }.is_ok() {
        out += &format!("pos={},{}\n", rc.left, rc.top);
    }
    for p in &app.patterns {
        out += &format!("pattern={}\n", p);
    }
    for (project, c) in &app.conf {
        if c.color < 0 && c.label.is_empty() {
            continue;
        }
        out += &format!("p={}\t{}\t{}\n", project, c.color, c.label);
    }
    let _ = std::fs::write(&app.cfg_path, out);
}

// ---------- перечисление окон ----------
fn extract_project(title: &str, pattern: &str) -> String {
    let core = title.strip_suffix(pattern).unwrap_or(title);
    let seg = core.rsplit(" - ").next().unwrap_or(core);
    seg.trim_start_matches(['●', '•', '*', ' ']).trim().to_string()
}

extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return BOOL(1);
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        let n = GetWindowTextW(hwnd, &mut buf);
        if n <= 0 {
            return BOOL(1);
        }
        let title = String::from_utf16_lossy(&buf[..n as usize]);
        let v = &mut *(lparam.0 as *mut Vec<(HWND, String)>);
        v.push((hwnd, title));
        BOOL(1)
    }
}

fn refresh_items(app: &mut App) {
    let mut raw: Vec<(HWND, String)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut raw as *mut _ as isize));
    }
    let mut items: Vec<Item> = Vec::new();
    for (hwnd, title) in raw {
        for pat in &app.patterns {
            if title.ends_with(pat.as_str()) {
                let project = extract_project(&title, pat);
                if !project.is_empty() {
                    items.push(Item { hwnd, project });
                }
                break;
            }
        }
    }
    items.sort_by(|a, b| {
        a.project
            .cmp(&b.project)
            .then((a.hwnd.0 as usize).cmp(&(b.hwnd.0 as usize)))
    });
    app.items = items;
}

// ---------- активация чужого окна ----------
fn activate(target: HWND) {
    unsafe {
        if IsIconic(target).as_bool() {
            let _ = ShowWindow(target, SW_RESTORE);
        }
        let fg = GetForegroundWindow();
        let cur = GetCurrentThreadId();
        let other = GetWindowThreadProcessId(fg, None);
        let _ = AttachThreadInput(cur, other, BOOL(1));
        let _ = BringWindowToTop(target);
        let _ = SetForegroundWindow(target);
        let _ = SetFocus(target);
        let _ = AttachThreadInput(cur, other, BOOL(0));
    }
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

// ---------- отрисовка ----------
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

unsafe fn paint(hwnd: HWND, app: &App) {
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
        let (_, r, g, b) = PALETTE[app.color_idx(&it.project)];
        fill(
            mem,
            RECT { left: 10, top: cy, right: 10 + SWATCH, bottom: cy + SWATCH },
            (r, g, b),
        );
        // имя проекта
        SelectObject(mem, app.font_main);
        SetTextColor(mem, rgb(C_TXT.0, C_TXT.1, C_TXT.2));
        let label = app.label(&it.project);
        let right_pad = if label.is_empty() { 10 } else { 96 };
        dt(
            mem,
            &it.project,
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

// ---------- размер окна по числу строк ----------
unsafe fn resize(hwnd: HWND, app: &mut App) {
    let n = app.items.len().max(1) as i32;
    let h = HEAD + ROW * n;
    if h != app.last_h {
        app.last_h = h;
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            W,
            h,
            SWP_NOMOVE | SWP_NOACTIVATE,
        );
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

fn row_at(y: i32, n: usize) -> i32 {
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

// ---------- оконная процедура ----------
extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TIMER => {
                APP.with(|c| {
                    if let Some(app) = c.borrow_mut().as_mut() {
                        refresh_items(app);
                        resize(hwnd, app);
                    }
                });
                let _ = InvalidateRect(hwnd, None, BOOL(0));
                LRESULT(0)
            }
            WM_PAINT => {
                APP.with(|c| {
                    if let Some(app) = c.borrow().as_ref() {
                        paint(hwnd, app);
                    }
                });
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (_, y) = xy(lp);
                let new = APP.with(|c| {
                    c.borrow()
                        .as_ref()
                        .map(|a| row_at(y, a.items.len()))
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
                if y < HEAD {
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
                let target = APP.with(|c| {
                    let a = c.borrow();
                    let a = a.as_ref()?;
                    let i = row_at(y, a.items.len());
                    if i >= 0 {
                        Some(a.items[i as usize].hwnd)
                    } else {
                        None
                    }
                });
                if let Some(t) = target {
                    activate(t);
                }
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                let (_, y) = xy(lp);
                let idx = APP.with(|c| {
                    c.borrow()
                        .as_ref()
                        .map(|a| row_at(y, a.items.len()))
                        .unwrap_or(-1)
                });
                if idx >= 0 {
                    APP.with(|c| {
                        if let Some(a) = c.borrow_mut().as_mut() {
                            a.menu_target = idx as usize;
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
                        save_config(app, hwnd);
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
        a.items.get(a.menu_target).map(|it| it.project.clone())
    });
    let Some(project) = project else { return };

    if (ID_COLOR_BASE..ID_COLOR_BASE + PALETTE.len()).contains(&id) {
        APP.with(|c| {
            if let Some(a) = c.borrow_mut().as_mut() {
                a.set_color(&project, id - ID_COLOR_BASE);
                save_config(a, hwnd);
            }
        });
        unsafe {
            let _ = InvalidateRect(hwnd, None, BOOL(0));
        }
    } else if id == ID_LABEL {
        let (hinst, cur) = APP.with(|c| {
            let a = c.borrow();
            let a = a.as_ref().unwrap();
            (a.hinst, a.label(&project))
        });
        if let Some(s) = prompt_text(hwnd, hinst, &cur) {
            APP.with(|c| {
                if let Some(a) = c.borrow_mut().as_mut() {
                    a.set_label(&project, s.trim().to_string());
                    save_config(a, hwnd);
                }
            });
            unsafe {
                let _ = InvalidateRect(hwnd, None, BOOL(0));
            }
        }
    } else if id == ID_LABEL_CLEAR {
        APP.with(|c| {
            if let Some(a) = c.borrow_mut().as_mut() {
                a.set_label(&project, String::new());
                save_config(a, hwnd);
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
        let (patterns, conf, pos) = load_config(&cfg_path);

        let mut app = App {
            hinst,
            items: Vec::new(),
            conf,
            patterns,
            cfg_path,
            font_main: make_font(-16, 600),
            font_small: make_font(-13, 400),
            hover: -1,
            menu_target: 0,
            last_h: 0,
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
        let n = app.items.len().max(1) as i32;
        let h = HEAD + ROW * n;
        let (x, y) = pos.unwrap_or((sw - W - 20, 40));

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            cls,
            w!("ClaudeBar"),
            WS_POPUP,
            x,
            y,
            W,
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
