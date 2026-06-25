// FILE: src/signal.rs
// VERSION: 1.1.0
// START_MODULE_CONTRACT
//   PURPOSE: «Звоночек» завершения ИИ: читать файлы-сигналы из %APPDATA%\claudebar\signals\, отдавать проекты для подсветки, гасить сигнал при фокусе окна проекта.
//   SCOPE: путь папки сигналов, парсинг .signal (cwd проекта), ключ проекта (basename) + полный cwd, наборы «звенящих» (basename и cwd), сброс по фокусу с матчем по полному пути.
//   DEPENDS: M-WINENUM (сопоставление сигнала с открытым окном по полному пути WinItem.path, иначе по basename)
//   LINKS: M-SIGNAL
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   Signal            - активный сигнал: путь файла, ключ проекта (basename, lower), полный cwd (lower)
//   signal_dir        - путь к %APPDATA%\claudebar\signals (создаётся при отсутствии)
//   parse_signal      - извлечь cwd проекта из содержимого .signal
//   project_key       - basename(cwd) в нижнем регистре — fallback-ключ сопоставления
//   should_clear      - чистое: сигнал гасится, если окно в фокусе — этот проект (по полному пути, иначе basename)
//   list_signals      - прочитать активные сигналы из папки
//   bell_keys         - множество «звенящих» basename-ключей для paint (fallback)
//   bell_cwds         - множество полных cwd активных сигналов — точная подсветка по пути (Phase-15)
//   reconcile         - удалить .signal, чьё окно проекта сейчас foreground (по полному пути)
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.1.0 - Phase-15 step-3: матч звоночка по полному cwd == WinItem.path (fallback basename); чинит коллизию одноимённых (D-06). Signal += cwd; should_clear(sig_cwd,sig_key,fg_path,fg_key); bell_cwds.
//   v1.0.0 - Phase-4 Step 1: модуль сигналов «звоночка» (файловый IPC из Claude Code).
// END_CHANGE_SUMMARY

use std::collections::HashSet;
use std::path::PathBuf;

use windows::Win32::Foundation::HWND;

use crate::win_enum::WinItem;

pub struct Signal {
    pub path: PathBuf,
    pub key: String, // имя проекта (lower) = basename(cwd) — fallback-матч
    pub cwd: String, // полный cwd проекта (lower) — точный матч по пути (Phase-15)
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
//   PURPOSE: Решить, гасить ли сигнал: окно в фокусе — этот проект. По полному пути (точно), иначе по basename.
//   INPUTS: { sig_cwd: &str - полный cwd сигнала (lower); sig_key: &str - basename (lower); fg_path: Option<&str> - путь окна в фокусе; fg_key: &str - basename имени окна (lower) }
//   OUTPUTS: { bool }
//   SIDE_EFFECTS: none
// END_CONTRACT: should_clear
pub fn should_clear(sig_cwd: &str, sig_key: &str, fg_path: Option<&str>, fg_key: &str) -> bool {
    match fg_path {
        Some(p) => p.eq_ignore_ascii_case(sig_cwd), // точный матч по полному пути (Phase-15)
        None => fg_key == sig_key,                  // окно без пути -> fallback на basename
    }
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
            out.push(Signal { path: p, key, cwd: cwd.to_lowercase() });
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

// START_CONTRACT: bell_cwds
//   PURPOSE: Множество полных cwd (lower) активных сигналов — для точной подсветки окна по пути (Phase-15).
//   INPUTS: {}
//   OUTPUTS: { HashSet<String> - cwd проектов (lower) с активным сигналом }
//   SIDE_EFFECTS: чтение каталога signals
// END_CONTRACT: bell_cwds
pub fn bell_cwds() -> HashSet<String> {
    list_signals().into_iter().map(|s| s.cwd).collect()
}

// START_CONTRACT: reconcile
//   PURPOSE: Удалить .signal, чьё окно проекта сейчас на переднем плане (сброс по фокусу).
//   INPUTS: { items: &[WinItem] - открытые окна; fg: HWND - окно в фокусе }
//   OUTPUTS: { () }
//   SIDE_EFFECTS: удаляет файлы-сигналы
// END_CONTRACT: reconcile
pub fn reconcile(items: &[WinItem], fg: HWND) {
    let Some(fgw) = items.iter().find(|it| it.hwnd == fg) else { return };
    let fg_path = fgw.path.as_deref();
    let fg_key = fgw.name.to_lowercase();
    // START_BLOCK_CLEAR_FOCUSED
    for s in list_signals() {
        if should_clear(&s.cwd, &s.key, fg_path, &fg_key) {
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
    fn should_clear_by_full_path() {
        // точный матч по полному пути: одноимённые в разных путях НЕ путаются
        assert!(should_clear("d:\\a\\claudebar", "claudebar", Some("D:\\a\\claudebar"), "claudebar")); // регистронезависимо
        assert!(!should_clear("d:\\a\\claudebar", "claudebar", Some("E:\\b\\claudebar"), "claudebar")); // другой путь -> НЕ гасит
        // окно без пути -> fallback на basename
        assert!(should_clear("d:\\a\\claudebar", "claudebar", None, "claudebar"));
        assert!(!should_clear("d:\\a\\claudebar", "claudebar", None, "other"));
    }
}
