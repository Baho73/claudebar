// FILE: src/win_enum.rs
// VERSION: 1.2.0
// START_MODULE_CONTRACT
//   PURPOSE: Перечисление окон, определение приложения по процессу И классу окна, извлечение имени проекта/документа.
//   SCOPE: EnumWindows, имя процесса и класс окна по HWND, snapshot процессов и разрешение shell-клиента консоли (conhost), извлечение имени по AppDef (вкл. Whole), фильтр по процессу+классу и сортировка в WinItem.
//   DEPENDS: M-CONFIG (AppDef/NameMode)
//   LINKS: M-WINENUM
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   WinItem        - окно: hwnd, индекс приложения, имя проекта/документа
//   list_windows   - собрать (hwnd, title, proc, class) видимых окон; для консоли proc = shell-клиент
//   match_windows  - фильтр по AppDef (процесс ∈ proc∪proc_alts И class) -> Vec<WinItem>, отсортировано
//   extract_name   - чистое извлечение имени из заголовка по правилу приложения (вкл. Whole)
//   process_name   - имя exe (нижний регистр) по HWND
//   window_class   - класс окна (GetClassNameW)
//   process_snapshot - карта pid -> (имя exe, parent pid) (Toolhelp), раз на опрос
//   resolve_shell / console_client - реальный shell консоли (cmd/powershell/pwsh) через conhost
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.2.0 - Phase-10 Step 2: класс окна (GetClassNameW); snapshot процессов и console_client (cmd/PowerShell через conhost); list_windows -> (hwnd,title,proc,class); match по процессу+классу (proc_alts/class).
//   v1.1.1 - fix: extract_name поддерживает DocumentLast (MS Project: имя файла в последнем сегменте).
//   v1.1.0 - Phase-2 Step 1: приложение по процессу (process_name), AppDef-правила имени, WinItem с индексом приложения.
//   v1.0.0 - Выделено из монолита main.rs (Phase-1, Step 2).
// END_CHANGE_SUMMARY

use std::collections::HashMap;

use windows::core::PWSTR;
use windows::Win32::Foundation::{BOOL, CloseHandle, HWND, LPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    IsWindowVisible,
};

use crate::config::{AppDef, NameMode};

pub struct WinItem {
    pub hwnd: HWND,
    pub app: usize, // индекс в Config.apps
    pub name: String,
    pub path: Option<String>, // полный путь проекта из заголовка (Project + настроенный ${rootPath}) — Phase-15
    pub ordinal: u64, // порядковый № первого появления окна за сессию (для сортировки recent) — Phase-16; 0 = ещё не задан
}

// START_CONTRACT: extract_name
//   PURPOSE: Имя проекта/документа из заголовка окна по правилу приложения.
//   INPUTS: { title: &str - заголовок; app: &AppDef - правило (Project{suffix} | Document) }
//   OUTPUTS: { String - имя }
//   SIDE_EFFECTS: none
// END_CONTRACT: extract_name
pub fn extract_name(title: &str, app: &AppDef) -> String {
    let seg = match &app.mode {
        NameMode::Project { suffix } => {
            let core = title.strip_suffix(suffix.as_str()).unwrap_or(title);
            core.rsplit(" - ").next().unwrap_or(core)
        }
        NameMode::Document => title.split(" - ").next().unwrap_or(title),
        NameMode::DocumentLast => title.rsplit(" - ").next().unwrap_or(title),
        NameMode::Whole => title,
    };
    seg.trim_start_matches(['●', '•', '*', ' ']).trim().to_string()
}

// START_CONTRACT: extract_project_path
//   PURPOSE: Полный путь проекта из заголовка редактора (если настроен показывать ${rootPath}) — Phase-15.
//   INPUTS: { title: &str - заголовок окна }
//   OUTPUTS: { Option<String> - сегмент-путь (X:\... или UNC \\...), иначе None }
//   SIDE_EFFECTS: none
// END_CONTRACT: extract_project_path
pub fn extract_project_path(title: &str) -> Option<String> {
    // сегменты по " - "; берём первый, похожий на путь (имя файла за путь не принимается)
    title.split(" - ").map(str::trim).find(|s| is_path(s)).map(|s| s.to_string())
}

// Похоже ли на путь Windows: диск X:\ / X:/, либо UNC \\server\...
fn is_path(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || s.starts_with("\\\\")
}

// Имя проекта = последний сегмент пути (сохраняя регистр) — для показа.
fn path_basename(p: &str) -> String {
    p.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or(p).to_string()
}

// START_CONTRACT: process_name
//   PURPOSE: Имя исполняемого файла (нижний регистр) для процесса окна.
//   INPUTS: { hwnd: HWND }
//   OUTPUTS: { Option<String> - напр. "winword.exe", или None при отсутствии прав/ошибке }
//   SIDE_EFFECTS: открывает и закрывает HANDLE процесса
// END_CONTRACT: process_name
pub fn process_name(hwnd: HWND) -> Option<String> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, BOOL(0), pid).ok()?;
        let mut buf = [0u16; 260];
        let mut sz = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_FORMAT(0), PWSTR(buf.as_mut_ptr()), &mut sz);
        let _ = CloseHandle(h);
        if r.is_err() || sz == 0 {
            return None;
        }
        let full = String::from_utf16_lossy(&buf[..sz as usize]);
        let base = full.rsplit(['\\', '/']).next().unwrap_or(&full).to_lowercase();
        Some(base)
    }
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

// Классические shell-клиенты консоли и хост-процессы консоли (conhost).
const SHELLS: [&str; 3] = ["cmd.exe", "powershell.exe", "pwsh.exe"];
const CONHOSTS: [&str; 2] = ["conhost.exe", "openconsole.exe"];
const CONSOLE_CLASS: &str = "ConsoleWindowClass";

// START_CONTRACT: window_class
//   PURPOSE: Класс окна (имя оконного класса) по HWND.
//   INPUTS: { hwnd: HWND }
//   OUTPUTS: { String - напр. "CabinetWClass" / "ConsoleWindowClass", пусто при ошибке }
//   SIDE_EFFECTS: none
// END_CONTRACT: window_class
pub fn window_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid
}

// START_CONTRACT: process_snapshot
//   PURPOSE: Снимок процессов: pid -> (имя exe в нижнем регистре, parent pid).
//   INPUTS: {}
//   OUTPUTS: { HashMap<u32, (String, u32)> }
//   SIDE_EFFECTS: CreateToolhelp32Snapshot (один раз на опрос)
// END_CONTRACT: process_snapshot
fn process_snapshot() -> HashMap<u32, (String, u32)> {
    let mut map = HashMap::new();
    unsafe {
        let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(h) => h,
            Err(_) => return map,
        };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                let end = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..end]).to_lowercase();
                map.insert(entry.th32ProcessID, (name, entry.th32ParentProcessID));
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    map
}

// START_CONTRACT: resolve_shell
//   PURPOSE: Реальный shell-клиент консоли (cmd/powershell/pwsh) по pid окна через дерево процессов.
//   INPUTS: { pid: u32 - процесс окна (может быть conhost); snap: &HashMap<u32,(String,u32)> }
//   OUTPUTS: { Option<String> - имя shell-exe или None }
//   SIDE_EFFECTS: none (чистая)
// END_CONTRACT: resolve_shell
fn resolve_shell(pid: u32, snap: &HashMap<u32, (String, u32)>) -> Option<String> {
    let (name, parent) = snap.get(&pid)?;
    if SHELLS.iter().any(|s| s.eq_ignore_ascii_case(name)) {
        return Some(name.clone());
    }
    if CONHOSTS.iter().any(|c| c.eq_ignore_ascii_case(name)) {
        // окном владеет conhost — его родитель это shell-клиент
        if let Some((pname, _)) = snap.get(parent) {
            if SHELLS.iter().any(|s| s.eq_ignore_ascii_case(pname)) {
                return Some(pname.clone());
            }
        }
    }
    None
}

fn console_client(hwnd: HWND, snap: &HashMap<u32, (String, u32)>) -> Option<String> {
    resolve_shell(window_pid(hwnd), snap)
}

// START_CONTRACT: list_windows
//   PURPOSE: Собрать видимые окна с заголовком, именем процесса и классом окна.
//   INPUTS: {}
//   OUTPUTS: { Vec<(HWND, String, String, String)> - (окно, заголовок, процесс, класс) }
//   SIDE_EFFECTS: EnumWindows + OpenProcess на каждое окно + один Toolhelp-снимок
// END_CONTRACT: list_windows
pub fn list_windows() -> Vec<(HWND, String, String, String)> {
    let mut raw: Vec<(HWND, String)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut raw as *mut _ as isize));
    }
    let snap = process_snapshot();
    raw.into_iter()
        .filter_map(|(hwnd, title)| {
            let mut proc = process_name(hwnd)?;
            let class = window_class(hwnd);
            // классическая консоль: окном владеет conhost — определяем реальный shell-клиент
            if class.eq_ignore_ascii_case(CONSOLE_CLASS) {
                if let Some(shell) = console_client(hwnd, &snap) {
                    proc = shell;
                }
            }
            Some((hwnd, title, proc, class))
        })
        .collect()
}

// Сопоставление окна с приложением: процесс ∈ proc∪proc_alts И (class отсутствует ИЛИ совпал).
fn app_matches(app: &AppDef, proc: &str, class: &str) -> bool {
    let proc_ok = app.proc.eq_ignore_ascii_case(proc)
        || app.proc_alts.iter().any(|p| p.eq_ignore_ascii_case(proc));
    let class_ok = app.class.as_deref().map_or(true, |c| c.eq_ignore_ascii_case(class));
    proc_ok && class_ok
}

// START_CONTRACT: match_windows
//   PURPOSE: Сопоставить окна с приложениями по процессу И классу окна, извлечь имена.
//   INPUTS: { raw: &[(HWND, String, String, String)] - (окно, заголовок, процесс, класс); apps: &[AppDef] }
//   OUTPUTS: { Vec<WinItem> - отсортировано по (приложение, имя, hwnd) }
//   SIDE_EFFECTS: none
// END_CONTRACT: match_windows
pub fn match_windows(raw: &[(HWND, String, String, String)], apps: &[AppDef]) -> Vec<WinItem> {
    let mut items: Vec<WinItem> = Vec::new();
    // START_BLOCK_MATCH_BY_PROCESS
    for (hwnd, title, proc, class) in raw {
        if let Some(i) = apps.iter().position(|a| app_matches(a, proc, class)) {
            // путь — только для редакторов (Project) с настроенным ${rootPath} в заголовке
            let path = matches!(apps[i].mode, NameMode::Project { .. })
                .then(|| extract_project_path(title))
                .flatten();
            // имя: при известном пути — его basename (заголовок мог сменить формат), иначе обычное правило
            let name = match &path {
                Some(p) => path_basename(p),
                None => extract_name(title, &apps[i]),
            };
            if !name.is_empty() {
                items.push(WinItem { hwnd: *hwnd, app: i, name, path, ordinal: 0 });
            }
        }
    }
    // END_BLOCK_MATCH_BY_PROCESS
    items.sort_by(|a, b| {
        a.app
            .cmp(&b.app)
            .then(a.name.cmp(&b.name))
            .then((a.hwnd.0 as usize).cmp(&(b.hwnd.0 as usize)))
    });
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_apps;

    fn h(n: usize) -> HWND {
        HWND(n as *mut core::ffi::c_void)
    }

    #[test]
    fn extract_name_editor_takes_project() {
        let apps = default_apps();
        let code = &apps[0]; // VS Code, Project{suffix}
        assert_eq!(
            extract_name("main.rs - ConstructMan - Visual Studio Code", code),
            "ConstructMan"
        );
        let cursor = &apps[1];
        assert_eq!(extract_name("ksp.md - Cursor", cursor), "ksp.md");
    }

    #[test]
    fn extract_project_path_from_title() {
        // путь в среднем сегменте (формат "${rootName} - ${rootPath} - ${appName}")
        assert_eq!(
            extract_project_path("claudebar - D:\\Python\\claudebar - Visual Studio Code").as_deref(),
            Some("D:\\Python\\claudebar")
        );
        assert_eq!(extract_project_path("\\\\srv\\share\\proj - Cursor").as_deref(), Some("\\\\srv\\share\\proj"));
        assert_eq!(extract_project_path("main.rs - claudebar - Visual Studio Code"), None); // нет пути
        assert_eq!(extract_project_path("Договор.docx - Word"), None); // имя файла не путь
        // match_windows: имя = basename пути, path заполнен
        let raw = [(h(1), "x - D:\\a\\claudebar - Visual Studio Code".to_string(), "code.exe".to_string(), String::new())];
        let items = match_windows(&raw, &default_apps());
        assert_eq!(items[0].name, "claudebar");
        assert_eq!(items[0].path.as_deref(), Some("D:\\a\\claudebar"));
    }

    #[test]
    fn extract_name_office_takes_document() {
        let apps = default_apps();
        let word = &apps[2]; // Word, Document
        assert_eq!(extract_name("Договор.docx - Word", word), "Договор.docx");
        let excel = &apps[3];
        assert_eq!(extract_name("Счет-Договор_ИП_Пономарев.xlsx - Excel", excel), "Счет-Договор_ИП_Пономарев.xlsx");
        // несохранённый
        assert_eq!(extract_name("Документ1 - Word", word), "Документ1");
    }

    #[test]
    fn extract_name_msproject_takes_last_segment() {
        // регрессия: заголовок MS Project — "App - Файл" (имя приложения первым)
        let apps = default_apps();
        let proj = &apps[4]; // MS Project, DocumentLast
        assert_eq!(
            extract_name("Project профессиональный - Задание на КСП_v02.mpp", proj),
            "Задание на КСП_v02.mpp"
        );
    }

    #[test]
    fn match_windows_groups_by_app_and_sorts() {
        let apps = default_apps();
        let raw = vec![
            (h(7), "Смета.xlsx - Excel".to_string(), "EXCEL.EXE".to_string(), String::new()),
            (h(3), "a - Zeta - Visual Studio Code".to_string(), "Code.exe".to_string(), String::new()),
            (h(9), "блокнот".to_string(), "notepad.exe".to_string(), String::new()), // не отслеживается
            (h(5), "Договор.docx - Word".to_string(), "winword.exe".to_string(), String::new()),
        ];
        let got = match_windows(&raw, &apps);
        // notepad отброшен, остальные сгруппированы по индексу приложения (Code=0, Word=2, Excel=3)
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].app, 0); // VS Code
        assert_eq!(got[0].name, "Zeta");
        assert_eq!(got[1].app, 2); // Word
        assert_eq!(got[1].name, "Договор.docx");
        assert_eq!(got[2].app, 3); // Excel
        assert_eq!(got[2].name, "Смета.xlsx");
    }

    #[test]
    fn match_windows_class_filter_explorer() {
        let apps = default_apps();
        let expl = apps.iter().position(|a| a.block == "Проводник").unwrap();
        // explorer.exe + CabinetWClass -> Проводник (имя = весь заголовок); со Shell_TrayWnd отброшен
        let raw = vec![
            (h(1), "claudebar".to_string(), "explorer.exe".to_string(), "CabinetWClass".to_string()),
            (h(2), "Панель задач".to_string(), "explorer.exe".to_string(), "Shell_TrayWnd".to_string()),
        ];
        let got = match_windows(&raw, &apps);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].app, expl);
        assert_eq!(got[0].name, "claudebar");
    }

    #[test]
    fn match_windows_console_cmd_vs_powershell() {
        let apps = default_apps();
        let cmd = apps.iter().position(|a| a.block == "Командная строка").unwrap();
        let ps = apps.iter().position(|a| a.block == "PowerShell").unwrap();
        // proc уже разрешён до shell-клиента (см. console_client); класс — консольный
        let raw = vec![
            (h(1), "C:\\Windows\\system32\\cmd.exe".to_string(), "cmd.exe".to_string(), "ConsoleWindowClass".to_string()),
            (h(2), "Windows PowerShell".to_string(), "powershell.exe".to_string(), "ConsoleWindowClass".to_string()),
            (h(3), "pwsh".to_string(), "pwsh.exe".to_string(), "ConsoleWindowClass".to_string()),
        ];
        let got = match_windows(&raw, &apps);
        assert_eq!(got.len(), 3);
        assert!(got.iter().filter(|w| w.app == cmd).count() == 1);
        assert!(got.iter().filter(|w| w.app == ps).count() == 2); // powershell + pwsh
    }

    #[test]
    fn extract_name_whole_keeps_full_title() {
        let apps = default_apps();
        let expl = apps.iter().find(|a| a.block == "Проводник").unwrap();
        // Whole не режет заголовок по " - "
        assert_eq!(extract_name("Папка - A - B", expl), "Папка - A - B");
    }

    #[test]
    fn resolve_shell_via_conhost_parent_or_direct() {
        let mut snap: HashMap<u32, (String, u32)> = HashMap::new();
        // окном владеет conhost(100), его родитель cmd(50)
        snap.insert(50, ("cmd.exe".into(), 0));
        snap.insert(100, ("conhost.exe".into(), 50));
        assert_eq!(resolve_shell(100, &snap).as_deref(), Some("cmd.exe"));
        // окно у клиента напрямую: powershell(200)
        snap.insert(200, ("powershell.exe".into(), 0));
        assert_eq!(resolve_shell(200, &snap).as_deref(), Some("powershell.exe"));
        // pwsh через conhost(101)
        snap.insert(60, ("pwsh.exe".into(), 0));
        snap.insert(101, ("conhost.exe".into(), 60));
        assert_eq!(resolve_shell(101, &snap).as_deref(), Some("pwsh.exe"));
        // conhost с не-shell родителем -> None (окно не показывается)
        snap.insert(70, ("python.exe".into(), 0));
        snap.insert(102, ("conhost.exe".into(), 70));
        assert_eq!(resolve_shell(102, &snap), None);
        // неизвестный pid -> None
        assert_eq!(resolve_shell(999, &snap), None);
    }
}
