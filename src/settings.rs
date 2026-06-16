// FILE: src/settings.rs
// VERSION: 1.2.0
// START_MODULE_CONTRACT
//   PURPOSE: Настройки и сведения панели: нативный выбор шрифта (ChooseFontW), предзаполненный текущей гарнитурой/кеглем/начертанием; окно «О программе» с версией и контактами автора.
//   SCOPE: choose_font (модальный диалог -> (face, size, weight) или None), parse_face (чистое: lfFaceName -> String), about_text (чистое: текст «О программе»), show_about (модальный MessageBox).
//   DEPENDS: none
//   LINKS: M-SETTINGS
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   choose_font  - ChooseFontW: модальный выбор шрифта, предзаполнен текущим face/size/weight; -> Option<(String, i32, i32)>
//   parse_face   - чистое: срез u16 (lfFaceName, null-terminated) -> String
//   set_face     - записать гарнитуру в lfFaceName (приватный помощник)
//   about_text   - чистое: собрать текст окна «О программе» из версии (+ TELEGRAM/GITHUB_URL)
//   show_about   - модальный MessageBox «О программе» (версия, Telegram, GitHub)
//   wide         - &str -> UTF-16 с завершающим \0 (приватный помощник для Win32-строк)
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.2.0 - Phase-11: пункт «О программе» в меню ⚙ — окно с версией, Telegram (@IvanPonomarev) и GitHub. about_text (чистое, тестируемо) + show_about (MessageBoxW).
//   v1.1.0 - fix(grace-fix): choose_font принимает/возвращает вес; lf.lfWeight предзаполняется текущим (стиль больше не сбрасывается); флаги канонические (CF_SCREENFONTS|CF_INITTOLOGFONTSTRUCT).
//   v1.0.0 - Phase-9 Step 2: новый модуль настроек; выбор шрифта через ChooseFontW.
// END_CHANGE_SUMMARY

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{DEFAULT_CHARSET, LOGFONTW};
use windows::Win32::UI::Controls::Dialogs::{
    ChooseFontW, CHOOSEFONTW, CF_INITTOLOGFONTSTRUCT, CF_SCREENFONTS,
};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};

// Контакты автора, показываемые в окне «О программе».
pub const TELEGRAM: &str = "@IvanPonomarev";
pub const GITHUB_URL: &str = "github.com/Baho73/claudebar";

// START_CONTRACT: parse_face
//   PURPOSE: Имя гарнитуры из массива lfFaceName (UTF-16, null-terminated) в String.
//   INPUTS: { buf: &[u16] }
//   OUTPUTS: { String - без хвостовых \0 }
//   SIDE_EFFECTS: none
// END_CONTRACT: parse_face
pub fn parse_face(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

// Записать гарнитуру в lfFaceName (LF_FACESIZE=32: до 31 символа + завершающий \0).
fn set_face(lf: &mut LOGFONTW, face: &str) {
    let n = lf.lfFaceName.len();
    let mut i = 0;
    for u in face.encode_utf16().take(n - 1) {
        lf.lfFaceName[i] = u;
        i += 1;
    }
    while i < n {
        lf.lfFaceName[i] = 0;
        i += 1;
    }
}

// START_CONTRACT: choose_font
//   PURPOSE: Открыть нативный диалог выбора шрифта, предзаполненный текущей гарнитурой/кеглем/начертанием.
//   INPUTS: { parent: HWND; cur_face: &str; cur_size: i32; cur_weight: i32 }
//   OUTPUTS: { Option<(String, i32, i32)> - (гарнитура, кегль px, вес) или None при отмене }
//   SIDE_EFFECTS: модальный диалог (кратко блокирует поток), читает выбор пользователя
// END_CONTRACT: choose_font
pub fn choose_font(parent: HWND, cur_face: &str, cur_size: i32, cur_weight: i32) -> Option<(String, i32, i32)> {
    unsafe {
        // START_BLOCK_INIT_LOGFONT
        let mut lf = LOGFONTW::default();
        lf.lfHeight = -cur_size.abs();
        lf.lfWeight = cur_weight; // предзаполнить текущим начертанием (иначе стиль сбрасывался)
        lf.lfCharSet = DEFAULT_CHARSET;
        set_face(&mut lf, cur_face);
        // END_BLOCK_INIT_LOGFONT
        let mut cf = CHOOSEFONTW {
            lStructSize: std::mem::size_of::<CHOOSEFONTW>() as u32,
            hwndOwner: parent,
            lpLogFont: &mut lf as *mut LOGFONTW,
            // CF_INITTOLOGFONTSTRUCT: диалог берёт face/size/weight из lf для предвыбора
            Flags: CF_SCREENFONTS | CF_INITTOLOGFONTSTRUCT,
            ..Default::default()
        };
        // START_BLOCK_SHOW_DIALOG
        if ChooseFontW(&mut cf).as_bool() {
            let face = parse_face(&lf.lfFaceName);
            // lfHeight отрицательный (высота символа); храним положительный px-кегль
            let size = lf.lfHeight.abs().max(6);
            let weight = if lf.lfWeight > 0 { lf.lfWeight } else { cur_weight };
            Some((face, size, weight))
        } else {
            None
        }
        // END_BLOCK_SHOW_DIALOG
    }
}

// START_CONTRACT: about_text
//   PURPOSE: Собрать текст окна «О программе» из версии и контактов автора.
//   INPUTS: { version: &str }
//   OUTPUTS: { String - многострочный текст с версией, Telegram и GitHub }
//   SIDE_EFFECTS: none
// END_CONTRACT: about_text
pub fn about_text(version: &str) -> String {
    format!(
        "ClaudeBar v{version}\n\n\
         Всегда-поверх панель-переключатель окон: список окон редакторов, Office, \
         терминалов и Проводника по приложениям. Клик — переключиться на окно.\n\n\
         Автор: Иван Пономарёв\n\
         Telegram: {TELEGRAM}\n\
         GitHub: {GITHUB_URL}"
    )
}

// &str -> UTF-16 с завершающим \0 для Win32-строковых аргументов.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// START_CONTRACT: show_about
//   PURPOSE: Показать модальное окно «О программе» (версия из CARGO_PKG_VERSION, контакты автора).
//   INPUTS: { parent: HWND }
//   OUTPUTS: { () }
//   SIDE_EFFECTS: модальный MessageBox (кратко блокирует поток до нажатия ОК)
// END_CONTRACT: show_about
pub fn show_about(parent: HWND) {
    let text = wide(&about_text(env!("CARGO_PKG_VERSION")));
    let caption = wide("О программе");
    unsafe {
        MessageBoxW(parent, PCWSTR(text.as_ptr()), PCWSTR(caption.as_ptr()), MB_OK | MB_ICONINFORMATION);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn about_text_has_version_and_contacts() {
        let t = about_text("0.4.0");
        assert!(t.contains("ClaudeBar v0.4.0"), "версия в тексте");
        assert!(t.contains(TELEGRAM), "Telegram-контакт в тексте");
        assert!(t.contains(GITHUB_URL), "ссылка на GitHub в тексте");
    }

    #[test]
    fn parse_face_trims_trailing_nulls() {
        // "Iosevka Fixed" в буфере 32 с хвостовыми нулями
        let mut buf = [0u16; 32];
        for (i, u) in "Iosevka Fixed".encode_utf16().enumerate() {
            buf[i] = u;
        }
        assert_eq!(parse_face(&buf), "Iosevka Fixed");
        // пустой буфер -> пустая строка
        assert_eq!(parse_face(&[0u16; 32]), "");
        // без завершающего нуля -> весь срез
        let full: Vec<u16> = "ABC".encode_utf16().collect();
        assert_eq!(parse_face(&full), "ABC");
    }
}
