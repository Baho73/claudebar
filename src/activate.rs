// FILE: src/activate.rs
// VERSION: 1.1.0
// START_MODULE_CONTRACT
//   PURPOSE: Управление чужим окном: надёжно вывести на передний план и штатно закрыть.
//   SCOPE: restore + AttachThreadInput-трюк + SetForegroundWindow + SetFocus; close = PostMessage WM_CLOSE.
//   DEPENDS: none
//   LINKS: M-ACTIVATE
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   activate - вывести окно по HWND на передний план
//   close    - закрыть окно штатно (PostMessage WM_CLOSE; приложение само спросит про сохранение)
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.1.0 - Phase-6 Step 2: close(hwnd) = PostMessageW(WM_CLOSE) для кнопки ✕.
//   v1.0.0 - Выделено из монолита main.rs (Phase-1, Step 3). Паритет v0.1.
// END_CHANGE_SUMMARY

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, PostMessageW,
    SetForegroundWindow, ShowWindow, SW_RESTORE, WM_CLOSE,
};

// START_CONTRACT: activate
//   PURPOSE: Вывести окно target на передний план (восстановить, если свёрнуто).
//   INPUTS: { target: HWND - целевое окно }
//   OUTPUTS: { () }
//   SIDE_EFFECTS: меняет активное окно/фокус; временно подключает ввод потоков
//   LINKS: вызывается из M-RENDER при ЛКМ по строке окна
// END_CONTRACT: activate
pub fn activate(target: HWND) {
    unsafe {
        // START_BLOCK_RESTORE_IF_ICONIC
        if IsIconic(target).as_bool() {
            let _ = ShowWindow(target, SW_RESTORE);
        }
        // END_BLOCK_RESTORE_IF_ICONIC
        // START_BLOCK_SET_FOREGROUND
        let fg = GetForegroundWindow();
        let cur = GetCurrentThreadId();
        let other = GetWindowThreadProcessId(fg, None);
        let _ = AttachThreadInput(cur, other, BOOL(1));
        let _ = BringWindowToTop(target);
        let _ = SetForegroundWindow(target);
        let _ = SetFocus(target);
        let _ = AttachThreadInput(cur, other, BOOL(0));
        // END_BLOCK_SET_FOREGROUND
    }
}

// START_CONTRACT: close
//   PURPOSE: Закрыть окно штатно (PostMessage WM_CLOSE) — приложение само спросит про сохранение.
//   INPUTS: { target: HWND - целевое окно }
//   OUTPUTS: { () }
//   SIDE_EFFECTS: посылает WM_CLOSE окну (асинхронно, не блокирует панель)
//   LINKS: вызывается из M-MAIN при ЛКМ по зоне ✕ строки окна
// END_CONTRACT: close
pub fn close(target: HWND) {
    unsafe {
        let _ = PostMessageW(target, WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}
