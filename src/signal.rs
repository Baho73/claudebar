// FILE: src/signal.rs
// VERSION: 1.2.0
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
//   is_stale          - чистое: устарел ли .busy по mtime (фильтр зависших) — Phase-17
//   list_ext          - чтение сигналов по расширению (signal|busy) с опц. staleness-фильтром
//   busy_keys/busy_cwds - наборы проектов с активным .busy (индикатор работы) — Phase-17
//   reconcile         - удалить .signal, чьё окно проекта сейчас foreground (по полному пути)
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.2.0 - Phase-17 step-1: чтение .busy (индикатор работы) — list_ext(ext, ttl) + busy_keys/busy_cwds + is_stale (фильтр зависших по mtime >600с). list_signals переведён на list_ext.
//   v1.1.0 - Phase-15 step-3: матч звоночка по полному cwd == WinItem.path (fallback basename); чинит коллизию одноимённых (D-06). Signal += cwd; should_clear(sig_cwd,sig_key,fg_path,fg_key); bell_cwds.
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

// .busy старше этого возраста (с) считается зависшим (Claude убит без Stop) и игнорируется — Phase-17.
pub const BUSY_STALE_SECS: u64 = 600;

// START_CONTRACT: is_stale
//   PURPOSE: Чистое: устарел ли файл-сигнал (mtime старше ttl от now) — фильтр зависших .busy (Phase-17).
//   INPUTS: { mtime_secs: u64; now_secs: u64; ttl: u64 }
//   OUTPUTS: { bool }
//   SIDE_EFFECTS: none
// END_CONTRACT: is_stale
pub fn is_stale(mtime_secs: u64, now_secs: u64, ttl: u64) -> bool {
    now_secs.saturating_sub(mtime_secs) > ttl
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn file_mtime_secs(p: &std::path::Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// START_CONTRACT: list_ext
//   PURPOSE: Прочитать активные сигналы заданного расширения (signal|busy); ttl=Some -> отфильтровать зависшие по mtime.
//   INPUTS: { ext: &str - расширение без точки; ttl: Option<u64> - порог staleness (None = без фильтра) }
//   OUTPUTS: { Vec<Signal> - путь, ключ (basename lower), полный cwd (lower) }
//   SIDE_EFFECTS: чтение каталога signals
// END_CONTRACT: list_ext
fn list_ext(ext: &str, ttl: Option<u64>) -> Vec<Signal> {
    let dir = match signal_dir() {
        Some(d) => d,
        None => return Vec::new(),
    };
    let now = now_secs();
    let mut out = Vec::new();
    // START_BLOCK_SCAN_SIGNALS
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case(ext)) != Some(true) {
                continue;
            }
            if let Some(ttl) = ttl {
                if is_stale(file_mtime_secs(&p), now, ttl) {
                    continue; // зависший .busy -> пропускаем
                }
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

// Активные .signal (звоночек) — без staleness-фильтра.
pub fn list_signals() -> Vec<Signal> {
    list_ext("signal", None)
}

// Активные .busy (индикатор работы) — с фильтром зависших по mtime.
fn list_busy() -> Vec<Signal> {
    list_ext("busy", Some(BUSY_STALE_SECS))
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

// START_CONTRACT: busy_keys
//   PURPOSE: Множество basename-ключей (lower) проектов с активным .busy — индикатор работы (fallback) — Phase-17.
//   INPUTS: {}
//   OUTPUTS: { HashSet<String> }
//   SIDE_EFFECTS: чтение каталога signals
// END_CONTRACT: busy_keys
pub fn busy_keys() -> HashSet<String> {
    list_busy().into_iter().map(|s| s.key).collect()
}

// START_CONTRACT: busy_cwds
//   PURPOSE: Множество полных cwd (lower) с активным .busy (staleness-фильтр) — бегущие точки по пути — Phase-17.
//   INPUTS: {}
//   OUTPUTS: { HashSet<String> }
//   SIDE_EFFECTS: чтение каталога signals
// END_CONTRACT: busy_cwds
pub fn busy_cwds() -> HashSet<String> {
    list_busy().into_iter().map(|s| s.cwd).collect()
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

    #[test]
    fn is_stale_by_mtime() {
        assert!(!is_stale(1000, 1500, 600)); // 500с < 600 -> свежий
        assert!(is_stale(1000, 1700, 600)); // 700с > 600 -> устарел
        assert!(!is_stale(1700, 1000, 600)); // mtime в будущем -> не устарел (saturating)
        assert!(!is_stale(1000, 1600, 600)); // ровно 600 -> ещё не устарел (строго >)
    }
}
