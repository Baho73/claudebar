// FILE: src/settings.rs
// VERSION: 1.0.0
// START_MODULE_CONTRACT
//   PURPOSE: Настройки панели: нативный выбор шрифта (ChooseFontW), предзаполненный текущей гарнитурой/кеглем.
//   SCOPE: choose_font (модальный диалог -> (face, size) или None), parse_face (чистое: lfFaceName -> String).
//   DEPENDS: none
//   LINKS: M-SETTINGS
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   choose_font  - ChooseFontW: модальный выбор шрифта, предзаполнен текущим; -> Option<(String, i32)>
//   parse_face   - чистое: срез u16 (lfFaceName, null-terminated) -> String
//   set_face     - записать гарнитуру в lfFaceName (приватный помощник)
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.0.0 - Phase-9 Step 2: новый модуль настроек; выбор шрифта через ChooseFontW.
// END_CHANGE_SUMMARY

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{DEFAULT_CHARSET, LOGFONTW};
use windows::Win32::UI::Controls::Dialogs::{
    ChooseFontW, CHOOSEFONTW, CF_FORCEFONTEXIST, CF_INITTOLOGFONTSTRUCT, CF_NOSCRIPTSEL, CF_SCREENFONTS,
};

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
//   PURPOSE: Открыть нативный диалог выбора шрифта, предзаполненный текущей гарнитурой/кеглем.
//   INPUTS: { parent: HWND; cur_face: &str; cur_size: i32 }
//   OUTPUTS: { Option<(String, i32)> - (гарнитура, кегль px) или None при отмене }
//   SIDE_EFFECTS: модальный диалог (кратко блокирует поток), читает выбор пользователя
// END_CONTRACT: choose_font
pub fn choose_font(parent: HWND, cur_face: &str, cur_size: i32) -> Option<(String, i32)> {
    unsafe {
        // START_BLOCK_INIT_LOGFONT
        let mut lf = LOGFONTW::default();
        lf.lfHeight = -cur_size.abs();
        lf.lfWeight = 400;
        lf.lfCharSet = DEFAULT_CHARSET;
        set_face(&mut lf, cur_face);
        // END_BLOCK_INIT_LOGFONT
        let mut cf = CHOOSEFONTW {
            lStructSize: std::mem::size_of::<CHOOSEFONTW>() as u32,
            hwndOwner: parent,
            lpLogFont: &mut lf as *mut LOGFONTW,
            iPointSize: cur_size.abs() * 10,
            Flags: CF_SCREENFONTS | CF_INITTOLOGFONTSTRUCT | CF_FORCEFONTEXIST | CF_NOSCRIPTSEL,
            ..Default::default()
        };
        // START_BLOCK_SHOW_DIALOG
        if ChooseFontW(&mut cf).as_bool() {
            let face = parse_face(&lf.lfFaceName);
            // lfHeight отрицательный (высота символа); храним положительный px-кегль
            let size = lf.lfHeight.abs().max(6);
            Some((face, size))
        } else {
            None
        }
        // END_BLOCK_SHOW_DIALOG
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
