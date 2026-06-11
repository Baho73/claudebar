// FILE: src/signal.rs
// VERSION: 1.0.0
// START_MODULE_CONTRACT
//   PURPOSE: «Звоночек» завершения ИИ: читать файлы-сигналы из %APPDATA%\claudebar\signals\, отдавать имена проектов для подсветки, гасить сигнал при фокусе окна проекта.
//   SCOPE: путь папки сигналов, парсинг .signal (cwd проекта), ключ проекта (basename), набор «звенящих» ключей, сброс по фокусу.
//   DEPENDS: M-WINENUM (сопоставление сигнала с открытым окном по имени проекта)
//   LINKS: M-SIGNAL
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   Signal            - активный сигнал: путь файла, ключ проекта (lower)
//   signal_dir        - путь к %APPDATA%\claudebar\signals (создаётся при отсутствии)
//   parse_signal      - извлечь cwd проекта из содержимого .signal
//   project_key       - basename(cwd) в нижнем регистре — ключ сопоставления со строкой окна
//   should_clear      - чистое: сигнал гасится, если его ключ == ключ окна в фокусе
//   list_signals      - прочитать активные сигналы из папки
//   bell_keys         - множество «звенящих» ключей проектов для paint
//   reconcile         - удалить .signal, чьё окно проекта сейчас foreground
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.0.0 - Phase-4 Step 1: модуль сигналов «звоночка» (файловый IPC из Claude Code).
// END_CHANGE_SUMMARY

use std::collections::HashSet;
use std::path::PathBuf;

use windows::Win32::Foundation::HWND;

use crate::win_enum::WinItem;

pub struct Signal {
    pub path: PathBuf,
    pub key: String, // имя проекта (lower) = basename(cwd)
}

// START_CONTRACT: signal_dir
//   PURPOSE: Путь к папке сигналов (%APPDATA%\claudebar\signals), создать при отсутствии.
//   INPUTS: {}
//   OUTPUTS: { Option<PathBuf> - None если %APPDATA% не задан }
//   SIDE_EFFECTS: создаёт каталог (create_dir_all)
// END_CONTRACT: signal_dir
pub fn signal_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    let dir = PathBuf::from(base).join("claudebar").join("signals");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

// START_CONTRACT: parse_signal
//   PURPOSE: Извлечь cwd проекта из содержимого .signal (строка cwd=... или первая непустая).
//   INPUTS: { content: &str - содержимое файла-сигнала }
//   OUTPUTS: { Option<String> - путь cwd или None }
//   SIDE_EFFECTS: none
// END_CONTRACT: parse_signal
pub fn parse_signal(content: &str) -> Option<String> {
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let v = t.strip_prefix("cwd=").unwrap_or(t).trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    None
}

// START_CONTRACT: project_key
//   PURPOSE: Ключ проекта для сопоставления со строкой окна: basename(cwd) в нижнем регистре.
//   INPUTS: { cwd: &str - путь проекта }
//   OUTPUTS: { String - имя последнего сегмента пути, lower }
//   SIDE_EFFECTS: none
// END_CONTRACT: project_key
pub fn project_key(cwd: &str) -> String {
    cwd.trim_end_matches(['\\', '/', ' '])
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(cwd)
        .to_lowercase()
}

// START_CONTRACT: should_clear
//   PURPOSE: Решить, гасить ли сигнал: его ключ совпал с ключом окна в фокусе.
//   INPUTS: { signal_key: &str; fg_key: Option<&str> - ключ проекта окна в фокусе }
//   OUTPUTS: { bool }
//   SIDE_EFFECTS: none
// END_CONTRACT: should_clear
pub fn should_clear(signal_key: &str, fg_key: Option<&str>) -> bool {
    fg_key == Some(signal_key)
}

// START_CONTRACT: list_signals
//   PURPOSE: Прочитать активные .signal из папки сигналов.
//   INPUTS: {}
//   OUTPUTS: { Vec<Signal> - путь, ключ проекта, mtime }
//   SIDE_EFFECTS: чтение каталога signals
// END_CONTRACT: list_signals
pub fn list_signals() -> Vec<Signal> {
    let dir = match signal_dir() {
        Some(d) => d,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    // START_BLOCK_SCAN_SIGNALS
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case("signal")) != Some(true) {
                continue;
            }
            let content = match std::fs::read_to_string(&p) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let cwd = match parse_signal(&content) {
                Some(c) => c,
                None => continue,
            };
            let key = project_key(&cwd);
            if key.is_empty() {
                continue;
            }
            out.push(Signal { path: p, key });
        }
    }
    // END_BLOCK_SCAN_SIGNALS
    out
}

// START_CONTRACT: bell_keys
//   PURPOSE: Множество «звенящих» ключей проектов для подсветки в paint.
//   INPUTS: {}
//   OUTPUTS: { HashSet<String> - ключи проектов (lower) с активным сигналом }
//   SIDE_EFFECTS: чтение каталога signals
// END_CONTRACT: bell_keys
pub fn bell_keys() -> HashSet<String> {
    list_signals().into_iter().map(|s| s.key).collect()
}

// START_CONTRACT: reconcile
//   PURPOSE: Удалить .signal, чьё окно проекта сейчас на переднем плане (сброс по фокусу).
//   INPUTS: { items: &[WinItem] - открытые окна; fg: HWND - окно в фокусе }
//   OUTPUTS: { () }
//   SIDE_EFFECTS: удаляет файлы-сигналы
// END_CONTRACT: reconcile
pub fn reconcile(items: &[WinItem], fg: HWND) {
    let fg_key = items.iter().find(|it| it.hwnd == fg).map(|it| it.name.to_lowercase());
    let Some(fg_key) = fg_key else { return };
    // START_BLOCK_CLEAR_FOCUSED
    for s in list_signals() {
        if should_clear(&s.key, Some(fg_key.as_str())) {
            let _ = std::fs::remove_file(&s.path);
        }
    }
    // END_BLOCK_CLEAR_FOCUSED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_signal_takes_cwd_or_first_line() {
        assert_eq!(parse_signal("d:\\Python\\claudebar\n"), Some("d:\\Python\\claudebar".to_string()));
        assert_eq!(parse_signal("cwd=d:\\X\ntime=123"), Some("d:\\X".to_string()));
        assert_eq!(parse_signal("\n\n  \n"), None);
        assert_eq!(parse_signal(""), None);
    }

    #[test]
    fn project_key_is_basename_lower() {
        assert_eq!(project_key("d:\\Python\\claudebar"), "claudebar");
        assert_eq!(project_key("d:/Python/ConstructMan/"), "constructman");
        assert_eq!(project_key("C:\\ws\\My Proj"), "my proj");
    }

    #[test]
    fn should_clear_only_on_exact_fg_key() {
        assert!(should_clear("proj", Some("proj")));
        assert!(!should_clear("proj", Some("other")));
        assert!(!should_clear("proj", None));
    }
}
