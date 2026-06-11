// FILE: src/icon.rs
// VERSION: 1.1.0
// START_MODULE_CONTRACT
//   PURPOSE: Иконка приложения для заголовка секции: взять у exe-файла приложения (SHGetFileInfo), кэшировать по приложению.
//   SCOPE: полный путь exe по окну, извлечение HICON из exe, ленивый кэш по индексу приложения.
//   DEPENDS: none (Win32 Threading + Shell + WindowsAndMessaging)
//   LINKS: M-ICON
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   section_icon  - HICON приложения по образцовому окну секции, с кэшем по app_idx
//   exe_path      - полный путь exe процесса окна
//   icon_from_path- иконка (small) из файла через SHGetFileInfo
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.1.0 - fix: иконки не отображались у Cursor/Word/Excel/MS Project. Источник переведён с иконки окна (WM_GETICON/class, кросс-процессно ненадёжен) на иконку exe-файла (SHGetFileInfo); негативный результат больше не кэшируется.
//   v1.0.0 - Phase-5 Step 1: иконки приложений в заголовках секций.
// END_CHANGE_SUMMARY

use std::cell::RefCell;
use std::collections::HashMap;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND};
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, HICON};

thread_local! {
    // app_idx -> raw HICON (isize). Кэшируем только успешный результат (негатив повторяем).
    static CACHE: RefCell<HashMap<usize, isize>> = RefCell::new(HashMap::new());
}

fn nonzero(h: isize) -> Option<HICON> {
    if h != 0 {
        Some(HICON(h as *mut core::ffi::c_void))
    } else {
        None
    }
}

// START_CONTRACT: section_icon
//   PURPOSE: Иконка приложения по образцовому окну секции, с кэшем по индексу приложения.
//   INPUTS: { app_idx: usize - индекс приложения (ключ кэша); sample: HWND - любое окно секции }
//   OUTPUTS: { Option<HICON> - иконка 16x16 или None }
//   SIDE_EFFECTS: открывает процесс окна, запрашивает иконку exe; пишет в thread-local кэш (только успех)
// END_CONTRACT: section_icon
pub fn section_icon(app_idx: usize, sample: HWND) -> Option<HICON> {
    if let Some(h) = CACHE.with(|c| c.borrow().get(&app_idx).copied()) {
        return nonzero(h);
    }
    // START_BLOCK_RESOLVE_ICON
    let icon = unsafe { exe_path(sample).and_then(|p| icon_from_path(&p)) };
    let raw = icon.map(|i| i.0 as isize).unwrap_or(0);
    if raw != 0 {
        CACHE.with(|c| {
            c.borrow_mut().insert(app_idx, raw);
        });
    }
    // END_BLOCK_RESOLVE_ICON
    nonzero(raw)
}

// START_CONTRACT: exe_path
//   PURPOSE: Полный путь exe процесса, которому принадлежит окно.
//   INPUTS: { hwnd: HWND }
//   OUTPUTS: { Option<String> - путь exe или None при отсутствии прав/ошибке }
//   SIDE_EFFECTS: открывает и закрывает HANDLE процесса
// END_CONTRACT: exe_path
unsafe fn exe_path(hwnd: HWND) -> Option<String> {
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
    Some(String::from_utf16_lossy(&buf[..sz as usize]))
}

// START_CONTRACT: icon_from_path
//   PURPOSE: Маленькая иконка файла/exe через SHGetFileInfo.
//   INPUTS: { path: &str - путь к файлу }
//   OUTPUTS: { Option<HICON> - иконка или None }
//   SIDE_EFFECTS: SHGetFileInfo (загружает иконку; владение HICON у вызывающего)
// END_CONTRACT: icon_from_path
unsafe fn icon_from_path(path: &str) -> Option<HICON> {
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut shfi = SHFILEINFOW::default();
    let r = SHGetFileInfoW(
        PCWSTR(wide.as_ptr()),
        FILE_FLAGS_AND_ATTRIBUTES(0),
        Some(&mut shfi),
        std::mem::size_of::<SHFILEINFOW>() as u32,
        SHGFI_ICON | SHGFI_SMALLICON,
    );
    if r != 0 && !shfi.hIcon.0.is_null() {
        Some(shfi.hIcon)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonzero_maps_handle() {
        assert!(nonzero(0).is_none());
        assert!(nonzero(42).is_some());
    }

    #[test]
    fn icon_from_path_extracts_system_exe_icon() {
        // regression: иконки не отображались у Cursor/Word/Excel/MS Project —
        // источник переведён на иконку exe через SHGetFileInfo. Системный exe должен дать иконку.
        let h = unsafe { icon_from_path("C:\\Windows\\explorer.exe") };
        assert!(h.is_some(), "explorer.exe должен дать иконку через SHGetFileInfo");
    }
}
