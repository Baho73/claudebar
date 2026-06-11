// FILE: src/win_enum.rs
// VERSION: 1.0.0
// START_MODULE_CONTRACT
//   PURPOSE: Перечисление видимых окон и сопоставление их с проектами по шаблонам заголовков.
//   SCOPE: EnumWindows-обёртка, извлечение имени проекта из заголовка, фильтр+сортировка совпадений.
//   DEPENDS: M-CONFIG (использует список шаблонов)
//   LINKS: M-WINENUM
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   list_windows    - собрать (hwnd, title) всех видимых окон с заголовком
//   match_windows   - чистый фильтр (hwnd, title) по шаблонам -> (hwnd, project), отсортировано
//   extract_project - чистое извлечение имени проекта из заголовка по шаблону
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.0.0 - Выделено из монолита main.rs (Phase-1, Step 2). Паритет v0.1.
// END_CHANGE_SUMMARY

use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, IsWindowVisible,
};

// START_CONTRACT: extract_project
//   PURPOSE: Имя проекта/документа из заголовка окна по совпавшему шаблону-суффиксу.
//   INPUTS: { title: &str - заголовок окна; pattern: &str - суффикс приложения }
//   OUTPUTS: { String - имя проекта (последний сегмент по " - ", без ведущих маркеров) }
//   SIDE_EFFECTS: none
// END_CONTRACT: extract_project
pub fn extract_project(title: &str, pattern: &str) -> String {
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

// START_CONTRACT: list_windows
//   PURPOSE: Собрать все видимые окна верхнего уровня с непустым заголовком.
//   INPUTS: {}
//   OUTPUTS: { Vec<(HWND, String)> - окна и их заголовки }
//   SIDE_EFFECTS: вызов EnumWindows
// END_CONTRACT: list_windows
pub fn list_windows() -> Vec<(HWND, String)> {
    let mut raw: Vec<(HWND, String)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut raw as *mut _ as isize));
    }
    raw
}

// START_CONTRACT: match_windows
//   PURPOSE: Отфильтровать окна по шаблонам заголовков и извлечь имена проектов.
//   INPUTS: { raw: &[(HWND, String)] - окна; patterns: &[String] - суффиксы приложений }
//   OUTPUTS: { Vec<(HWND, String)> - (окно, имя проекта), отсортировано по имени затем hwnd }
//   SIDE_EFFECTS: none
// END_CONTRACT: match_windows
pub fn match_windows(raw: &[(HWND, String)], patterns: &[String]) -> Vec<(HWND, String)> {
    let mut items: Vec<(HWND, String)> = Vec::new();
    // START_BLOCK_FILTER_BY_PATTERN
    for (hwnd, title) in raw {
        for pat in patterns {
            if title.ends_with(pat.as_str()) {
                let project = extract_project(title, pat);
                if !project.is_empty() {
                    items.push((*hwnd, project));
                }
                break;
            }
        }
    }
    // END_BLOCK_FILTER_BY_PATTERN
    items.sort_by(|a, b| {
        a.1.cmp(&b.1).then((a.0 .0 as usize).cmp(&(b.0 .0 as usize)))
    });
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: usize) -> HWND {
        HWND(n as *mut core::ffi::c_void)
    }

    #[test]
    fn extract_project_vscode_and_cursor() {
        assert_eq!(
            extract_project("main.rs - ConstructMan - Visual Studio Code", " - Visual Studio Code"),
            "ConstructMan"
        );
        assert_eq!(
            extract_project("ksp-naming-rules.md - Cursor", " - Cursor"),
            "ksp-naming-rules.md"
        );
    }

    #[test]
    fn extract_project_strips_unsaved_marker_and_handles_no_separator() {
        assert_eq!(
            extract_project("● file.rs - Proj - Visual Studio Code", " - Visual Studio Code"),
            "Proj"
        );
        // нет " - " -> весь остаток
        assert_eq!(extract_project("Proj - Cursor", " - Cursor"), "Proj");
        assert_eq!(extract_project("Solo", " - Visual Studio Code"), "Solo");
    }

    #[test]
    fn match_windows_filters_and_sorts() {
        let raw = vec![
            (h(5), "a.rs - Zeta - Visual Studio Code".to_string()),
            (h(3), "b - Cursor".to_string()),
            (h(9), "random window".to_string()),
        ];
        let pats = vec![" - Visual Studio Code".to_string(), " - Cursor".to_string()];
        let got = match_windows(&raw, &pats);
        assert_eq!(got.len(), 2);
        // сортировка по имени проекта: "Zeta" (Z=90) < "b" (98)
        assert_eq!(got[0].1, "Zeta");
        assert_eq!(got[1].1, "b");
    }

    #[test]
    fn match_windows_sorts_same_project_by_hwnd() {
        let raw = vec![
            (h(9), "x - P - Cursor".to_string()),
            (h(2), "y - P - Cursor".to_string()),
        ];
        let pats = vec![" - Cursor".to_string()];
        let got = match_windows(&raw, &pats);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0 .0 as usize, 2);
        assert_eq!(got[1].0 .0 as usize, 9);
    }
}
