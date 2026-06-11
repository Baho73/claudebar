// FILE: src/recent.rs
// VERSION: 1.2.0
// START_MODULE_CONTRACT
//   PURPOSE: Недавние элементы: документы Office (Windows Recent) и проекты редакторов (workspaceStorage); открытие через ShellExecute.
//   SCOPE: чтение Recent (.lnk) по расширению, чтение workspaceStorage редакторов, исключение открытых, открытие.
//   DEPENDS: M-CONFIG (AppDef: exts, editor_storage, proc)
//   LINKS: M-RECENT
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   RecentDoc      - недавний элемент: имя, индекс приложения, mtime, команда открытия
//   OpenCmd        - как открыть: Lnk(ярлык Recent) | Editor{exe, folder}
//   classify       - чистая классификация имени файла -> индекс Office-приложения
//   decode_file_uri- декодирование file:///... в путь Windows
//   extract_target - извлечь folder/workspace из workspace.json
//   list_recent    - собрать недавние (Office + редакторы), исключив открытые
//   open           - открыть элемент (ShellExecute)
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.2.0 - Phase-7 Step 1: жёсткий лимит 6 снят (STORE_LIMIT=50 на app); обрезку до 6 решает M-RENDER через «показать все».
//   v1.1.0 - недавние проекты редакторов (VS Code/Cursor) из workspaceStorage; OpenCmd для разных способов открытия.
//   v1.0.0 - Phase-3 Step 1: недавние документы из Windows Recent (без COM).
// END_CHANGE_SUMMARY

use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use windows::core::{w, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::config::AppDef;

#[derive(Clone)]
pub enum OpenCmd {
    Lnk(PathBuf),                       // ярлык Windows Recent
    Editor { exe: String, folder: String }, // запуск редактора с папкой проекта
}

pub struct RecentDoc {
    pub name: String,  // отображаемое имя (файл или папка проекта)
    pub app: usize,    // индекс в Config.apps
    pub mtime: u64,    // время последнего открытия
    pub open: OpenCmd, // как открыть
}

fn appdata() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

// START_CONTRACT: classify
//   PURPOSE: По имени файла определить Office-приложение (по расширению), отсеяв открытые.
//   INPUTS: { fname: &str; exts_by_app: &[Vec<String>]; open: &HashSet<String> }
//   OUTPUTS: { Option<usize> }
//   SIDE_EFFECTS: none
// END_CONTRACT: classify
pub fn classify(fname: &str, exts_by_app: &[Vec<String>], open: &HashSet<String>) -> Option<usize> {
    let (base, ext) = fname.rsplit_once('.')?;
    let ext = ext.to_lowercase();
    let app = exts_by_app
        .iter()
        .position(|exts| exts.iter().any(|e| e.eq_ignore_ascii_case(&ext)))?;
    if open.contains(&base.to_lowercase()) {
        return None;
    }
    Some(app)
}

// START_CONTRACT: decode_file_uri
//   PURPOSE: Преобразовать file:///d%3A/Path в путь Windows (d:\Path).
//   INPUTS: { uri: &str }
//   OUTPUTS: { String - путь Windows }
//   SIDE_EFFECTS: none
// END_CONTRACT: decode_file_uri
pub fn decode_file_uri(uri: &str) -> String {
    let s = uri
        .strip_prefix("file:///")
        .or_else(|| uri.strip_prefix("file://"))
        .unwrap_or(uri);
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(n) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(n);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).replace('/', "\\")
}

// START_CONTRACT: extract_target
//   PURPOSE: Достать file://-URI папки или workspace из содержимого workspace.json.
//   INPUTS: { json: &str }
//   OUTPUTS: { Option<String> - file:// URI }
//   SIDE_EFFECTS: none
// END_CONTRACT: extract_target
pub fn extract_target(json: &str) -> Option<String> {
    for key in ["\"folder\"", "\"workspace\""] {
        if let Some(k) = json.find(key) {
            let after = &json[k + key.len()..];
            if let Some(fs) = after.find("file://") {
                let tail = &after[fs..];
                if let Some(end) = tail.find('"') {
                    return Some(tail[..end].to_string());
                }
            }
        }
    }
    None
}

fn mtime_of(p: &std::path::Path) -> u64 {
    p.metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn collect_editor(app_idx: usize, exe: &str, storage: &str, open: &HashSet<String>, out: &mut Vec<RecentDoc>) {
    let base = match appdata() {
        Some(a) => a,
        None => return,
    };
    let dir = base.join(storage).join("User").join("workspaceStorage");
    // START_BLOCK_SCAN_WORKSPACES
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let wj = e.path().join("workspace.json");
            let json = match std::fs::read_to_string(&wj) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let uri = match extract_target(&json) {
                Some(u) => u,
                None => continue,
            };
            let path = decode_file_uri(&uri);
            if !std::path::Path::new(&path).exists() {
                continue; // проект удалён/перемещён
            }
            let name = path.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
            if name.is_empty() || open.contains(&name.to_lowercase()) {
                continue;
            }
            out.push(RecentDoc {
                name,
                app: app_idx,
                mtime: mtime_of(&e.path()),
                open: OpenCmd::Editor { exe: exe.to_string(), folder: path },
            });
        }
    }
    // END_BLOCK_SCAN_WORKSPACES
}

fn collect_office(apps: &[AppDef], open: &HashSet<String>, out: &mut Vec<RecentDoc>) {
    let exts_by_app: Vec<Vec<String>> = apps.iter().map(|a| a.exts.clone()).collect();
    if exts_by_app.iter().all(|e| e.is_empty()) {
        return;
    }
    let dir = match appdata() {
        Some(a) => a.join("Microsoft").join("Windows").join("Recent"),
        None => return,
    };
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case("lnk")) != Some(true) {
                continue;
            }
            let fname = match p.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            if let Some(app) = classify(&fname, &exts_by_app, open) {
                out.push(RecentDoc { name: fname, app, mtime: mtime_of(&p), open: OpenCmd::Lnk(p) });
            }
        }
    }
}

// START_CONTRACT: list_recent
//   PURPOSE: Недавние элементы всех приложений (Office-файлы + проекты редакторов), кроме открытых.
//   INPUTS: { apps: &[AppDef]; open: &HashSet<String> - basename(lower) открытых }
//   OUTPUTS: { Vec<RecentDoc> - по app, mtime убыв., дедуп по имени, не более STORE_LIMIT на приложение }
//   SIDE_EFFECTS: чтение каталогов Recent и workspaceStorage
// END_CONTRACT: list_recent
pub fn list_recent(apps: &[AppDef], open: &HashSet<String>) -> Vec<RecentDoc> {
    const STORE_LIMIT: usize = 50;
    let mut docs: Vec<RecentDoc> = Vec::new();
    for (i, app) in apps.iter().enumerate() {
        if let Some(storage) = &app.editor_storage {
            collect_editor(i, &app.proc, storage, open, &mut docs);
        }
    }
    collect_office(apps, open, &mut docs);

    docs.sort_by(|a, b| a.app.cmp(&b.app).then(b.mtime.cmp(&a.mtime)));
    // дедуп по (app, имя) и лимит на приложение
    let n_apps = apps.len();
    let mut count = vec![0usize; n_apps.max(1)];
    let mut seen: HashSet<(usize, String)> = HashSet::new();
    docs.into_iter()
        .filter(|d| {
            if !seen.insert((d.app, d.name.to_lowercase())) {
                return false;
            }
            let c = &mut count[d.app.min(n_apps.saturating_sub(1))];
            if *c < STORE_LIMIT {
                *c += 1;
                true
            } else {
                false
            }
        })
        .collect()
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

// START_CONTRACT: open
//   PURPOSE: Открыть недавний элемент в ассоциированном приложении.
//   INPUTS: { cmd: &OpenCmd }
//   OUTPUTS: { () }
//   SIDE_EFFECTS: ShellExecuteW (запуск приложения)
// END_CONTRACT: open
pub fn open(cmd: &OpenCmd) {
    match cmd {
        OpenCmd::Lnk(p) => {
            let f = wide(p.as_os_str());
            unsafe {
                ShellExecuteW(None, w!("open"), PCWSTR(f.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
            }
        }
        OpenCmd::Editor { exe, folder } => {
            let f = wide(OsStr::new(exe));
            let params: Vec<u16> = format!("\"{}\"", folder).encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                ShellExecuteW(None, w!("open"), PCWSTR(f.as_ptr()), PCWSTR(params.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exts() -> Vec<Vec<String>> {
        vec![
            vec![],
            vec![],
            vec!["docx".into(), "doc".into()],
            vec!["xlsx".into(), "xls".into()],
            vec!["mpp".into()],
        ]
    }

    #[test]
    fn classify_matches_extension_and_excludes_open() {
        let e = exts();
        let mut open = HashSet::new();
        assert_eq!(classify("Счет.xlsx", &e, &open), Some(3));
        assert_eq!(classify("Доклад.docx", &e, &open), Some(2));
        assert_eq!(classify("План.mpp", &e, &open), Some(4));
        assert_eq!(classify("readme.md", &e, &open), None);
        assert_eq!(classify("no_extension", &e, &open), None);
        open.insert("счет".to_string());
        assert_eq!(classify("Счет.xlsx", &e, &open), None);
    }

    #[test]
    fn decode_file_uri_handles_drive_and_cyrillic() {
        assert_eq!(
            decode_file_uri("file:///d%3A/Python/Test_2026.05.28"),
            "d:\\Python\\Test_2026.05.28"
        );
        // кириллица в percent-encoding (UTF-8)
        assert_eq!(decode_file_uri("file:///c%3A/%D0%9F%D1%80%D0%BE%D0%B5%D0%BA%D1%82"), "c:\\Проект");
    }

    #[test]
    fn extract_target_finds_folder_then_workspace() {
        assert_eq!(
            extract_target(r#"{"folder":"file:///d%3A/Python/ConstructMan"}"#),
            Some("file:///d%3A/Python/ConstructMan".to_string())
        );
        assert_eq!(
            extract_target(r#"{"workspace":"file:///c%3A/ws/my.code-workspace"}"#),
            Some("file:///c%3A/ws/my.code-workspace".to_string())
        );
        assert_eq!(extract_target(r#"{"other":"x"}"#), None);
    }
}
