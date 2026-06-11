// FILE: src/win_enum.rs
// VERSION: 1.1.0
// START_MODULE_CONTRACT
//   PURPOSE: Перечисление окон, определение приложения по процессу и извлечение имени проекта/документа.
//   SCOPE: EnumWindows, имя процесса по HWND, извлечение имени по AppDef, фильтр+сортировка в WinItem.
//   DEPENDS: M-CONFIG (AppDef/NameMode)
//   LINKS: M-WINENUM
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   WinItem       - окно: hwnd, индекс приложения, имя проекта/документа
//   list_windows  - собрать (hwnd, title, proc) видимых окон
//   match_windows - чистый фильтр по AppDef -> Vec<WinItem>, отсортировано
//   extract_name  - чистое извлечение имени из заголовка по правилу приложения
//   process_name  - имя exe (нижний регистр) по HWND
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.1.0 - Phase-2 Step 1: приложение по процессу (process_name), AppDef-правила имени, WinItem с индексом приложения.
//   v1.0.0 - Выделено из монолита main.rs (Phase-1, Step 2).
// END_CHANGE_SUMMARY

use windows::core::PWSTR;
use windows::Win32::Foundation::{BOOL, CloseHandle, HWND, LPARAM};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};

use crate::config::{AppDef, NameMode};

pub struct WinItem {
    pub hwnd: HWND,
    pub app: usize, // индекс в Config.apps
    pub name: String,
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
    };
    seg.trim_start_matches(['●', '•', '*', ' ']).trim().to_string()
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

// START_CONTRACT: list_windows
//   PURPOSE: Собрать видимые окна с заголовком и именем процесса.
//   INPUTS: {}
//   OUTPUTS: { Vec<(HWND, String, String)> - (окно, заголовок, имя процесса) }
//   SIDE_EFFECTS: EnumWindows + OpenProcess на каждое окно
// END_CONTRACT: list_windows
pub fn list_windows() -> Vec<(HWND, String, String)> {
    let mut raw: Vec<(HWND, String)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut raw as *mut _ as isize));
    }
    raw.into_iter()
        .filter_map(|(hwnd, title)| process_name(hwnd).map(|p| (hwnd, title, p)))
        .collect()
}

// START_CONTRACT: match_windows
//   PURPOSE: Сопоставить окна с приложениями по процессу и извлечь имена.
//   INPUTS: { raw: &[(HWND, String, String)] - (окно, заголовок, процесс); apps: &[AppDef] }
//   OUTPUTS: { Vec<WinItem> - отсортировано по (приложение, имя, hwnd) }
//   SIDE_EFFECTS: none
// END_CONTRACT: match_windows
pub fn match_windows(raw: &[(HWND, String, String)], apps: &[AppDef]) -> Vec<WinItem> {
    let mut items: Vec<WinItem> = Vec::new();
    // START_BLOCK_MATCH_BY_PROCESS
    for (hwnd, title, proc) in raw {
        if let Some(i) = apps.iter().position(|a| a.proc.eq_ignore_ascii_case(proc)) {
            let name = extract_name(title, &apps[i]);
            if !name.is_empty() {
                items.push(WinItem { hwnd: *hwnd, app: i, name });
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
    fn extract_name_office_takes_document() {
        let apps = default_apps();
        let word = &apps[2]; // Word, Document
        assert_eq!(extract_name("Договор.docx - Word", word), "Договор.docx");
        let excel = &apps[3];
        assert_eq!(extract_name("Смета.xlsx - Excel", excel), "Смета.xlsx");
        // несохранённый
        assert_eq!(extract_name("Документ1 - Word", word), "Документ1");
    }

    #[test]
    fn match_windows_groups_by_app_and_sorts() {
        let apps = default_apps();
        let raw = vec![
            (h(7), "Смета.xlsx - Excel".to_string(), "EXCEL.EXE".to_string()),
            (h(3), "a - Zeta - Visual Studio Code".to_string(), "Code.exe".to_string()),
            (h(9), "блокнот".to_string(), "notepad.exe".to_string()), // не отслеживается
            (h(5), "Договор.docx - Word".to_string(), "winword.exe".to_string()),
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
}
