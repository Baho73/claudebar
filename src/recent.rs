// FILE: src/recent.rs
// VERSION: 1.0.0
// START_MODULE_CONTRACT
//   PURPOSE: Недавние документы из Windows Recent по расширению, исключая открытые; открытие через ShellExecute.
//   SCOPE: чтение папки Recent (.lnk), классификация по расширению/приложению, исключение открытых, ShellExecuteW.
//   DEPENDS: M-CONFIG (AppDef.exts)
//   LINKS: M-RECENT
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   RecentDoc    - недавний документ: имя, путь к .lnk, индекс приложения, mtime
//   classify     - чистая классификация имени файла -> индекс приложения (или None)
//   list_recent  - собрать недавние документы из %APPDATA%\..\Recent, исключив открытые
//   open_doc     - открыть документ через ShellExecuteW(.lnk)
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.0.0 - Phase-3 Step 1: недавние документы из Windows Recent (без COM, ShellExecute по .lnk).
// END_CHANGE_SUMMARY

use std::collections::HashSet;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::core::PCWSTR;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

pub struct RecentDoc {
    #[allow(dead_code)] // используется в Phase-3 Step-2 (отрисовка)
    pub name: String, // имя файла, напр. "Счет.xlsx"
    #[allow(dead_code)] // используется в Phase-3 Step-2 (открытие по клику)
    pub lnk: PathBuf, // путь к ярлыку в Recent
    pub app: usize,   // индекс в Config.apps
    pub mtime: u64,   // время последнего открытия (mtime ярлыка)
}

fn recent_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Microsoft").join("Windows").join("Recent"))
}

// START_CONTRACT: classify
//   PURPOSE: По имени файла определить приложение (по расширению) и отсеять открытые сейчас.
//   INPUTS: { fname: &str - "Файл.ext"; exts_by_app: &[Vec<String>] - расширения каждого приложения; open: &HashSet<String> - basename(без ext, lower) открытых }
//   OUTPUTS: { Option<usize> - индекс приложения или None }
//   SIDE_EFFECTS: none
// END_CONTRACT: classify
pub fn classify(fname: &str, exts_by_app: &[Vec<String>], open: &HashSet<String>) -> Option<usize> {
    let (base, ext) = match fname.rsplit_once('.') {
        Some((b, e)) => (b, e),
        None => return None,
    };
    let ext = ext.to_lowercase();
    let app = exts_by_app
        .iter()
        .position(|exts| exts.iter().any(|e| e.eq_ignore_ascii_case(&ext)))?;
    if open.contains(&base.to_lowercase()) {
        return None;
    }
    Some(app)
}

// START_CONTRACT: list_recent
//   PURPOSE: Недавние документы отслеживаемых приложений из Windows Recent, кроме открытых.
//   INPUTS: { exts_by_app: &[Vec<String>]; open: &HashSet<String> }
//   OUTPUTS: { Vec<RecentDoc> - по app, mtime убыв., не более LIMIT на приложение }
//   SIDE_EFFECTS: чтение каталога Recent
// END_CONTRACT: list_recent
pub fn list_recent(exts_by_app: &[Vec<String>], open: &HashSet<String>) -> Vec<RecentDoc> {
    const LIMIT: usize = 6;
    let dir = match recent_dir() {
        Some(d) => d,
        None => return Vec::new(),
    };
    let mut docs: Vec<RecentDoc> = Vec::new();
    // START_BLOCK_SCAN_RECENT
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            let is_lnk = p.extension().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case("lnk"));
            if is_lnk != Some(true) {
                continue;
            }
            // file_stem у "Счет.xlsx.lnk" -> "Счет.xlsx"
            let fname = match p.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            if let Some(app) = classify(&fname, exts_by_app, open) {
                let mtime = e
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                docs.push(RecentDoc { name: fname, lnk: p, app, mtime });
            }
        }
    }
    // END_BLOCK_SCAN_RECENT
    docs.sort_by(|a, b| a.app.cmp(&b.app).then(b.mtime.cmp(&a.mtime)));
    // лимит на приложение
    let n_apps = exts_by_app.len();
    let mut count = vec![0usize; n_apps];
    docs.into_iter()
        .filter(|d| {
            let c = &mut count[d.app.min(n_apps.saturating_sub(1))];
            if *c < LIMIT {
                *c += 1;
                true
            } else {
                false
            }
        })
        .collect()
}

// START_CONTRACT: open_doc
//   PURPOSE: Открыть документ в ассоциированном приложении через ярлык Recent.
//   INPUTS: { lnk: &Path - путь к .lnk }
//   OUTPUTS: { () }
//   SIDE_EFFECTS: ShellExecuteW (запуск ассоциированного приложения)
// END_CONTRACT: open_doc
#[allow(dead_code)] // используется в Phase-3 Step-2 (клик по недавнему)
pub fn open_doc(lnk: &Path) {
    let wide: Vec<u16> = lnk.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    unsafe {
        ShellExecuteW(None, PCWSTR::null(), PCWSTR(wide.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exts() -> Vec<Vec<String>> {
        // как в default_apps: [VS Code, Cursor, Word, Excel, MS Project]
        vec![
            vec![],
            vec![],
            vec!["docx".into(), "doc".into(), "rtf".into()],
            vec!["xlsx".into(), "xls".into(), "csv".into()],
            vec!["mpp".into()],
        ]
    }

    #[test]
    fn classify_matches_extension() {
        let e = exts();
        let open = HashSet::new();
        assert_eq!(classify("Счет.xlsx", &e, &open), Some(3));
        assert_eq!(classify("Доклад.docx", &e, &open), Some(2));
        assert_eq!(classify("План.mpp", &e, &open), Some(4));
        assert_eq!(classify("readme.md", &e, &open), None); // не отслеживается
        assert_eq!(classify("no_extension", &e, &open), None);
    }

    #[test]
    fn classify_excludes_open_documents() {
        let e = exts();
        let mut open = HashSet::new();
        open.insert("счет".to_string()); // открыт сейчас (basename без ext, lower)
        assert_eq!(classify("Счет.xlsx", &e, &open), None);
        // другой файл того же приложения не исключается
        assert_eq!(classify("Другой.xlsx", &e, &open), Some(3));
    }

    #[test]
    fn classify_extension_case_insensitive() {
        let e = exts();
        let open = HashSet::new();
        assert_eq!(classify("ОТЧЕТ.XLSX", &e, &open), Some(3));
    }
}
