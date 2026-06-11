// FILE: src/icon.rs
// VERSION: 1.0.0
// START_MODULE_CONTRACT
//   PURPOSE: Иконка приложения для заголовка секции: взять у окна (WM_GETICON / class icon), кэшировать по приложению.
//   SCOPE: получение HICON по окну, ленивый кэш по индексу приложения.
//   DEPENDS: none (Win32 WindowsAndMessaging)
//   LINKS: M-ICON
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   section_icon - HICON приложения по образцовому окну секции, с кэшем по app_idx
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.0.0 - Phase-5 Step 1: иконки приложений в заголовках секций.
// END_CHANGE_SUMMARY

use std::cell::RefCell;
use std::collections::HashMap;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassLongPtrW, SendMessageW, GCLP_HICON, GCLP_HICONSM, HICON, WM_GETICON,
};

thread_local! {
    // app_idx -> raw HICON (isize); 0 = иконки нет. Кэш, чтобы не дёргать окно каждый кадр.
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
//   SIDE_EFFECTS: запрашивает иконку у окна; пишет в thread-local кэш
// END_CONTRACT: section_icon
pub fn section_icon(app_idx: usize, sample: HWND) -> Option<HICON> {
    if let Some(h) = CACHE.with(|c| c.borrow().get(&app_idx).copied()) {
        return nonzero(h);
    }
    let raw = unsafe { fetch_icon(sample) }.map(|i| i.0 as isize).unwrap_or(0);
    CACHE.with(|c| {
        c.borrow_mut().insert(app_idx, raw);
    });
    nonzero(raw)
}

// Иконки WM_GETICON / class icon принадлежат окну/классу — DestroyIcon не нужен.
unsafe fn fetch_icon(hwnd: HWND) -> Option<HICON> {
    // START_BLOCK_FETCH_ICON
    let r = SendMessageW(hwnd, WM_GETICON, WPARAM(2), LPARAM(0)); // 2 = ICON_SMALL2
    if r.0 != 0 {
        return Some(HICON(r.0 as *mut core::ffi::c_void));
    }
    let h = GetClassLongPtrW(hwnd, GCLP_HICONSM);
    if h != 0 {
        return Some(HICON(h as *mut core::ffi::c_void));
    }
    let h = GetClassLongPtrW(hwnd, GCLP_HICON);
    if h != 0 {
        return Some(HICON(h as *mut core::ffi::c_void));
    }
    None
    // END_BLOCK_FETCH_ICON
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonzero_maps_handle() {
        assert!(nonzero(0).is_none());
        assert!(nonzero(42).is_some());
    }
}
