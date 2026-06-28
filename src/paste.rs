// FILE: src/paste.rs
// VERSION: 1.1.0
// START_MODULE_CONTRACT
//   PURPOSE: Вставить текст в активное поле любого приложения: положить в буфер обмена и сымитировать Ctrl+V, не затерев буфер пользователя надолго.
//   SCOPE: paste_text (сохранить буфер -> set_clipboard -> SendInput Ctrl+V -> отложенно вернуть прежний буфер); set_clipboard/get_clipboard (CF_UNICODETEXT).
//   DEPENDS: none (Win32 DataExchange/Memory/KeyboardAndMouse — фичи уже подключены)
//   LINKS: M-PASTE
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   paste_text    - вставить текст в foreground-окно (Ctrl+V); пустой текст -> no-op; прежний буфер восстановить через ~400мс (если keep_clipboard)
//   set_clipboard - положить строку в буфер (CF_UNICODETEXT); GlobalFree на путях ошибки
//   get_clipboard - прочитать текущий текст буфера -> Option<String>
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.1.0 - Phase-20: paste_text(text, keep_clipboard). keep=true восстанавливает прежний буфер (как было), keep=false оставляет распознанный текст. Флаг из M-CONFIG.voice_keep_clipboard (⚙-галочка).
//   v1.0.0 - Phase-18 step-5: вставка надиктованного текста. Буфер открываем без окна-владельца
//                (OpenClipboard null), чтобы восстановление шло из фонового потока. Восстановление отложено на
//                ~400мс (целевое приложение успевает прочитать вставку). Win32, тестируется ручным smoke.
// END_CHANGE_SUMMARY

use std::time::Duration;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    VK_CONTROL, VK_V,
};

const CF_UNICODETEXT: u32 = 13;

// START_CONTRACT: get_clipboard
//   PURPOSE: Прочитать текущий текст буфера обмена (для сохранения перед вставкой).
//   INPUTS: {}
//   OUTPUTS: { Option<String> - None, если буфер пуст/недоступен/не текст }
//   SIDE_EFFECTS: открывает/закрывает буфер обмена
// END_CONTRACT: get_clipboard
pub unsafe fn get_clipboard() -> Option<String> {
    if OpenClipboard(HWND::default()).is_err() {
        return None;
    }
    let result = match GetClipboardData(CF_UNICODETEXT) {
        Ok(h) if !h.is_invalid() => {
            let ptr = GlobalLock(HGLOBAL(h.0)) as *const u16;
            if ptr.is_null() {
                None
            } else {
                let mut len = 0usize;
                while *ptr.add(len) != 0 {
                    len += 1;
                }
                let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
                let _ = GlobalUnlock(HGLOBAL(h.0));
                Some(s)
            }
        }
        _ => None,
    };
    let _ = CloseClipboard();
    result
}

// START_CONTRACT: set_clipboard
//   PURPOSE: Положить строку в буфер обмена (CF_UNICODETEXT). Безопасно к утечкам hmem.
//   INPUTS: { text: &str }
//   OUTPUTS: { bool - успех }
//   SIDE_EFFECTS: меняет буфер обмена ОС
// END_CONTRACT: set_clipboard
pub unsafe fn set_clipboard(text: &str) -> bool {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let Ok(hmem) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) else {
        return false;
    };
    let dst = GlobalLock(hmem);
    if dst.is_null() {
        let _ = GlobalFree(hmem);
        return false;
    }
    std::ptr::copy_nonoverlapping(wide.as_ptr(), dst as *mut u16, wide.len());
    let _ = GlobalUnlock(hmem);
    if OpenClipboard(HWND::default()).is_ok() {
        let _ = EmptyClipboard();
        // владение hmem уходит буферу ТОЛЬКО при успешном SetClipboardData; иначе освобождаем
        let ok = SetClipboardData(CF_UNICODETEXT, HANDLE(hmem.0)).is_ok();
        if !ok {
            let _ = GlobalFree(hmem);
        }
        let _ = CloseClipboard();
        ok
    } else {
        let _ = GlobalFree(hmem);
        false
    }
}

// Сымитировать нажатие Ctrl+V через SendInput (ctrl↓ v↓ v↑ ctrl↑).
unsafe fn send_ctrl_v() {
    fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }
    let inputs = [
        key(VK_CONTROL, false),
        key(VK_V, false),
        key(VK_V, true),
        key(VK_CONTROL, true),
    ];
    SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
}

// START_CONTRACT: paste_text
//   PURPOSE: Вставить текст туда, где стоит курсор (активное поле foreground-окна).
//   INPUTS: { text: &str; keep_clipboard: bool - true: вернуть прежний буфер после вставки; false: оставить распознанный текст }
//   OUTPUTS: { bool - true, если буфер выставлен и Ctrl+V отправлен; false при пустом тексте/ошибке }
//   SIDE_EFFECTS: меняет буфер обмена, шлёт ввод в активное окно; при keep_clipboard через ~400мс возвращает прежний буфер (фоновый поток)
//   LINKS: M-MAIN (вызывает на WM_APP_VOICE_DONE в UI-потоке, флаг из M-CONFIG.voice_keep_clipboard)
// END_CONTRACT: paste_text
pub unsafe fn paste_text(text: &str, keep_clipboard: bool) -> bool {
    if text.is_empty() {
        return false;
    }
    // прежний буфер нужен только если его восстанавливаем
    let prev = if keep_clipboard { get_clipboard() } else { None };
    if !set_clipboard(text) {
        return false;
    }
    send_ctrl_v();
    // Отложенно вернуть прежний буфер: целевое приложение успевает прочитать вставку.
    // ponytail: фиксированные 400мс; если приложение тормозит — поднять задержку.
    if let Some(prev) = prev {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            unsafe {
                set_clipboard(&prev);
            }
        });
    }
    true
}
