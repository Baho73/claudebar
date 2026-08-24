// FILE: src/menu.rs
// VERSION: 1.3.0
// START_MODULE_CONTRACT
//   PURPOSE: Контекст-меню окна/строки и ⚙-меню настроек + обработчик команд меню (цвет, метка, ссылка/проводник, галочки настроек).
//   SCOPE: show_menu / show_settings_menu (TrackPopupMenu + флаг MENU_ACTIVE), handle_command (все ID_*), copy_to_clipboard, open_in_explorer_select.
//   DEPENDS: M-STATE, M-CONFIG, M-SETTINGS, M-PROMPT, M-TOOLTIP, M-VOICE, M-MAIN (rebuild_rows/rebuild_fonts/spawn_files_index/run_live_search)
//   LINKS: M-MENU
//   ROLE: UI_COMPONENT
//   MAP_MODE: EXPORTS
//   NOTE: Вынесено из main.rs (рефактор god-object, FPF D-5). MENU_ACTIVE живёт здесь — wndproc читает его, чтобы тики таймера не закрывали модальный TrackPopupMenu (Phase-14 fix).
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   MENU_ACTIVE            - флаг «открыто модальное меню»: wndproc глушит WM_TIMER, иначе меню закрывается на первом тике
//   show_menu              - меню по ПКМ: ссылка/проводник + (не minimal) палитра цветов и метка
//   show_settings_menu     - ⚙-меню: шрифт, поиск в файлах, полные пути, сортировка, 4 голосовые галочки, о программе
//   handle_command         - обработчик всех команд меню (ID_SET_FONT..ID_STREAMING, ID_COPY_LINK/ID_OPEN_DIR, палитра, метка)
//   copy_to_clipboard      - положить текст в буфер обмена (CF_UNICODETEXT), корректное владение hmem (D-03)
//   (open_in_explorer_select проверяет существование пути и отклоняет кавычку — D-20/D-21)
//   open_in_explorer_select - открыть Проводник с выделением файла: explorer.exe /select,"<path>"
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.3.0 - M-MAIL: пункты «Открыть входящие (N)» и «Входящие разобраны» (видны только когда по папке есть неразобранное). «Разобраны» зовёт РОУТЕР командой maildonecmd= — панель в .inbox не лезет; без ключа пункт серый.
//   v1.2.0 - M-MAIL: пункт «Запустить Claude Code здесь» (ID_RUN_SESSION) -> spawn_session по шаблону sessioncmd= из ini; для документа берётся его папка.
//   v1.1.0 - FPF D-20/D-21: open_in_explorer_select проверяет существование пути (протухшая карта путей — до 3 мин) и падает на ближайшую живую папку-родителя вместо пустого окна «Библиотеки»; путь с кавычкой отклоняется (разорвал бы аргумент командной строки).
//   v1.0.0 - рефактор: меню и обработчик команд вынесены из main.rs в отдельный UI-модуль (M-MENU), без изменения поведения.
// END_CHANGE_SUMMARY

use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::PALETTE;
use crate::searchbox::set_search_cue;
use crate::tooltip::hide_tooltip;
use crate::{
    prompt, rebuild_fonts, rebuild_rows, run_live_search, settings, spawn_files_index, voice, APP,
    ID_TIP_TIMER,
};

// id команд меню
const ID_COLOR_BASE: usize = 1; // 1..=8
const ID_LABEL: usize = 20;
const ID_LABEL_CLEAR: usize = 21;
const ID_COPY_LINK: usize = 22; // меню окна: скопировать путь (Phase-14)
const ID_OPEN_DIR: usize = 23; // меню окна: открыть в проводнике (Phase-14)
const ID_RUN_SESSION: usize = 24; // меню: запустить сессию агента в папке проекта — M-MAIL
const ID_MAIL_OPEN: usize = 25; // меню: открыть папку .inbox проекта — M-MAIL
const ID_MAIL_DONE: usize = 26; // меню: «входящие разобраны» (просим роутер) — M-MAIL
const CF_UNICODETEXT: u32 = 13; // формат буфера обмена Win32 (clipboard CF_UNICODETEXT)
const ID_SET_FONT: usize = 30; // меню настроек: выбрать шрифт
const ID_ABOUT: usize = 31; // меню настроек: о программе
const ID_TOGGLE_FILES: usize = 32; // меню настроек: искать и в файлах (history)
const ID_FULLPATHS: usize = 33; // меню настроек: включить полные пути в заголовках редакторов (Phase-15)
const ID_SORT: usize = 34; // меню настроек: сортировка окон по времени (recent) vs по имени (alpha) — Phase-16
const ID_KEEP_CLIP: usize = 35; // меню настроек: сохранять прежний буфер обмена после диктовки — Phase-20
const ID_MIC_ALWAYS: usize = 36; // меню настроек: микрофон всегда включён (always-on + pre-roll) — Phase-22
const ID_TRAIL_SPACE: usize = 37; // меню настроек: пробел после надиктованной фразы — Phase-23
const ID_STREAMING: usize = 38; // меню настроек: стриминг длинной диктовки — Phase-24

// Пока открыто контекстное меню (модальный TrackPopupMenu) — WM_TIMER не трогает окно/тултип,
// иначе любой тик (dwell-тултип ~0.5с или общий refresh ~1с) закрывает меню. Phase-14 fix.
pub(crate) static MENU_ACTIVE: AtomicBool = AtomicBool::new(false);

pub(crate) unsafe fn show_menu(hwnd: HWND, minimal: bool) {
    let menu = CreatePopupMenu().unwrap_or_default();
    // Phase-14: ссылка/проводник — в начале меню (до палитры); серые, если путь не резолвится
    let has_link = APP.with(|c| c.borrow().as_ref().map(|a| a.menu_link.is_some()).unwrap_or(false));
    let lflag = if has_link { MF_STRING } else { MF_STRING | MF_GRAYED };
    let _ = AppendMenuW(menu, lflag, ID_COPY_LINK, w!("Скопировать ссылку"));
    let _ = AppendMenuW(menu, lflag, ID_OPEN_DIR, w!("Открыть в проводнике"));
    let _ = AppendMenuW(menu, lflag, ID_RUN_SESSION, w!("Запустить Claude Code здесь"));
    // Входящие (M-MAIL): пункты появляются только если по этой папке есть неразобранное.
    // «Разобраны» просит РОУТЕР (команда из ini) снять флаги — панель в .inbox не пишет.
    let mail = APP.with(|c| {
        let b = c.borrow();
        let a = b.as_ref()?;
        let (p, is_file) = a.menu_link.clone()?;
        let dir = if is_file {
            std::path::Path::new(&p).parent()?.display().to_string()
        } else {
            p
        };
        let ms = crate::mail::mails_for_row(&a.mails, Some(&dir), "");
        let m = crate::mail::merge_mails(&ms)?;
        Some((m.count, a.config.mail_done_cmd.trim().is_empty()))
    });
    if let Some((count, done_off)) = mail {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let open: Vec<u16> =
            format!("Открыть входящие ({count})").encode_utf16().chain(std::iter::once(0)).collect();
        let _ = AppendMenuW(menu, MF_STRING, ID_MAIL_OPEN, PCWSTR(open.as_ptr()));
        // без maildonecmd= в ini снимать нечем — пункт виден, но серый (объясняет, что настроить)
        let dflag = if done_off { MF_STRING | MF_GRAYED } else { MF_STRING };
        let _ = AppendMenuW(menu, dflag, ID_MAIL_DONE, w!("Входящие разобраны"));
    }
    if !minimal {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        for (i, p) in PALETTE.iter().enumerate() {
            let name: Vec<u16> = p.0.encode_utf16().chain(std::iter::once(0)).collect();
            let _ = AppendMenuW(menu, MF_STRING, ID_COLOR_BASE + i, PCWSTR(name.as_ptr()));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_LABEL, w!("Метка…"));
        let _ = AppendMenuW(menu, MF_STRING, ID_LABEL_CLEAR, w!("Убрать метку"));
    }
    track(hwnd, menu);
}

// Меню настроек панели (вызывается кликом «⚙» в шапке): выбор шрифта и «О программе».
pub(crate) unsafe fn show_settings_menu(hwnd: HWND) {
    let menu = CreatePopupMenu().unwrap_or_default();
    let _ = AppendMenuW(menu, MF_STRING, ID_SET_FONT, w!("Шрифт…"));
    let files_on = APP.with(|c| c.borrow().as_ref().map(|a| a.config.search_files).unwrap_or(false));
    let fflag = if files_on { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(menu, fflag, ID_TOGGLE_FILES, w!("Искать в файлах (history)"));
    let _ = AppendMenuW(menu, MF_STRING, ID_FULLPATHS, w!("Полные пути в заголовках редакторов"));
    let recent_on = APP.with(|c| c.borrow().as_ref().map(|a| a.config.is_sort_recent()).unwrap_or(false));
    let sflag = if recent_on { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(menu, sflag, ID_SORT, w!("Сортировка по времени (новые внизу)"));
    let keep_on = APP.with(|c| c.borrow().as_ref().map(|a| a.config.voice_keep_clipboard).unwrap_or(true));
    let kflag = if keep_on { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(menu, kflag, ID_KEEP_CLIP, w!("Сохранять прежний буфер обмена (диктовка)"));
    let mic_on = APP.with(|c| c.borrow().as_ref().map(|a| a.config.voice_always_on).unwrap_or(false));
    let mflag = if mic_on { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(menu, mflag, ID_MIC_ALWAYS, w!("Микрофон всегда включён (pre-roll)"));
    let trail_on = APP.with(|c| c.borrow().as_ref().map(|a| a.config.voice_trailing_space).unwrap_or(true));
    let tflag = if trail_on { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(menu, tflag, ID_TRAIL_SPACE, w!("Пробел после фразы (диктовка)"));
    let stream_on = APP.with(|c| c.borrow().as_ref().map(|a| a.config.voice_streaming).unwrap_or(false));
    let sflag = if stream_on { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(menu, sflag, ID_STREAMING, w!("Стриминг длинной диктовки"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
    let _ = AppendMenuW(menu, MF_STRING, ID_ABOUT, w!("О программе…"));
    track(hwnd, menu);
}

// Общий показ popup-меню: меню модальное, поэтому на время прячем тултип и глушим тики таймера
// (иначе dwell-тултип ~0.5с или общий refresh ~1с закроют меню). Phase-14 fix.
unsafe fn track(hwnd: HWND, menu: HMENU) {
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    hide_tooltip(hwnd);
    let _ = KillTimer(hwnd, ID_TIP_TIMER);
    MENU_ACTIVE.store(true, Ordering::Relaxed);
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(menu, TPM_LEFTALIGN | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
    MENU_ACTIVE.store(false, Ordering::Relaxed);
    let _ = DestroyMenu(menu);
}

pub(crate) fn handle_command(hwnd: HWND, id: usize) {
    // настройки: выбрать шрифт (не привязано к проекту)
    if id == ID_SET_FONT {
        let cur = APP.with(|c| c.borrow().as_ref().map(|a| (a.config.font_face.clone(), a.config.font_size, a.config.font_weight)));
        if let Some((face, size, weight)) = cur {
            // диалог модальный — borrow APP не держим, пока он открыт
            if let Some((nf, ns, nw)) = settings::choose_font(hwnd, &face, size, weight) {
                APP.with(|c| {
                    if let Some(a) = c.borrow_mut().as_mut() {
                        a.config.set_font(&nf, ns, nw);
                        a.config.save(hwnd);
                        rebuild_fonts(a);
                    }
                });
                unsafe {
                    let _ = InvalidateRect(hwnd, None, BOOL(0));
                }
            }
        }
        return;
    }
    // настройки: окно «О программе» (версия + контакты автора)
    if id == ID_ABOUT {
        settings::show_about(hwnd);
        return;
    }
    // настройки: включить полные пути в заголовках редакторов (window.title с ${rootPath})
    if id == ID_FULLPATHS {
        let res = settings::configure_editor_titles();
        let msg = if res.is_empty() {
            "Не найден %APPDATA% — настроить не удалось.".to_string()
        } else {
            let lines: Vec<String> = res.iter().map(|(e, s)| format!("• {}: {}", e, s)).collect();
            format!(
                "Заголовки редакторов:\n{}\n\nПерезапусти редактор, чтобы применилось. Бэкап: settings.json.claudebar-bak",
                lines.join("\n")
            )
        };
        let wmsg: Vec<u16> = msg.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            MessageBoxW(hwnd, PCWSTR(wmsg.as_ptr()), w!("Полные пути"), MB_OK | MB_ICONINFORMATION);
        }
        return;
    }
    // настройки: переключить сортировку окон (alpha <-> recent)
    if id == ID_SORT {
        APP.with(|c| {
            if let Some(a) = c.borrow_mut().as_mut() {
                let v = !a.config.is_sort_recent();
                a.config.set_sort_recent(v);
                a.config.save(hwnd);
                rebuild_rows(a);
            }
        });
        unsafe {
            let _ = InvalidateRect(hwnd, None, BOOL(0));
        }
        return;
    }
    // настройки: сохранять ли прежний буфер обмена после вставки диктовки — Phase-20
    if id == ID_KEEP_CLIP {
        APP.with(|c| {
            if let Some(a) = c.borrow_mut().as_mut() {
                a.config.voice_keep_clipboard = !a.config.voice_keep_clipboard;
                a.config.save(hwnd); // персист флага
            }
        });
        return;
    }
    // настройки: always-on микрофон (always-on + pre-roll) — Phase-22
    if id == ID_MIC_ALWAYS {
        APP.with(|c| {
            if let Some(a) = c.borrow_mut().as_mut() {
                // D-14: применить сначала; конфиг/галочку менять ТОЛЬКО при успехе (на Idle),
                // иначе тоггл во время записи разъедет ini и реальность.
                let want = !a.config.voice_always_on;
                if a.voice.set_always_on(want) {
                    a.config.voice_always_on = want;
                    a.config.save(hwnd); // персист флага
                } else {
                    voice::vlog("ID_MIC_ALWAYS: игнор — идёт запись (конфиг не тронут)");
                }
            }
        });
        return;
    }
    // настройки: пробел после надиктованной фразы (диктовка не липнет к точке) — Phase-23
    if id == ID_TRAIL_SPACE {
        APP.with(|c| {
            if let Some(a) = c.borrow_mut().as_mut() {
                a.config.voice_trailing_space = !a.config.voice_trailing_space;
                a.config.save(hwnd); // персист флага
            }
        });
        return;
    }
    // настройки: стриминг длинной диктовки (Phase-24)
    if id == ID_STREAMING {
        APP.with(|c| {
            if let Some(a) = c.borrow_mut().as_mut() {
                a.config.voice_streaming = !a.config.voice_streaming;
                a.config.save(hwnd); // персист флага
            }
        });
        return;
    }
    // настройки: переключить scope «+Файлы» (history)
    if id == ID_TOGGLE_FILES {
        let on = APP.with(|c| {
            c.borrow_mut()
                .as_mut()
                .map(|a| {
                    a.config.search_files = !a.config.search_files;
                    a.config.save(hwnd); // персист scope
                    a.config.search_files
                })
                .unwrap_or(false)
        });
        if on {
            spawn_files_index(hwnd); // построить индекс файлов в фоне
            unsafe { set_search_cue(w!("⏳ Индексирую файлы…")) };
        }
        run_live_search(hwnd); // пересчитать выдачу с учётом нового scope
        return;
    }
    // M-MAIL: открыть папку .inbox / попросить роутер разобрать входящие.
    if id == ID_MAIL_OPEN || id == ID_MAIL_DONE {
        let ctx = APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref()?;
            let (p, is_file) = a.menu_link.clone()?;
            let dir = if is_file {
                std::path::Path::new(&p).parent()?.display().to_string()
            } else {
                p
            };
            let ms = crate::mail::mails_for_row(&a.mails, Some(&dir), "");
            if ms.is_empty() {
                return None;
            }
            // папки всех вложенных проектов: «разобрать» надо каждый, а не первый попавшийся
            let cwds: Vec<String> = ms.iter().map(|m| m.cwd.clone()).collect();
            Some((cwds, a.config.mail_done_cmd.clone()))
        });
        let Some((cwds, done_cmd)) = ctx else { return };
        if id == ID_MAIL_OPEN {
            // несколько вложенных проектов -> открываем каждую папку входящих
            for cwd in &cwds {
                let dir = crate::mail::inbox_dir(cwd);
                let wide: Vec<u16> =
                    dir.display().to_string().encode_utf16().chain(std::iter::once(0)).collect();
                unsafe {
                    ShellExecuteW(None, w!("open"), PCWSTR(wide.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
                }
            }
        } else {
            // Роутер снимет флаг с писем и пересчитает .inbox — значок погаснет.
            // Панель только просит: писать в чужие файлы не её дело.
            for cwd in &cwds {
                crate::spawn_session(&done_cmd, cwd);
            }
        }
        return;
    }
    // M-MAIL: запустить сессию агента в папке проекта. Значок входящих при этом НЕ гаснет —
    // его гасит только разбор входящего (файл уходит из .inbox), а не открытие сессии.
    if id == ID_RUN_SESSION {
        let link = APP.with(|c| c.borrow().as_ref().and_then(|a| a.menu_link.clone()));
        let Some((path, is_file)) = link else { return };
        // цель — папка: для документа берём его каталог
        let dir = if is_file {
            std::path::Path::new(&path).parent().map(|p| p.display().to_string()).unwrap_or(path)
        } else {
            path
        };
        let cmd = APP.with(|c| c.borrow().as_ref().map(|a| a.config.session_cmd.clone()).unwrap_or_default());
        crate::spawn_session(&cmd, &dir);
        return;
    }
    // Phase-14: «Скопировать ссылку» / «Открыть в проводнике» — берут готовый a.menu_link
    if id == ID_COPY_LINK || id == ID_OPEN_DIR {
        let link = APP.with(|c| c.borrow().as_ref().and_then(|a| a.menu_link.clone()));
        let Some((path, is_file)) = link else { return };
        unsafe {
            if id == ID_COPY_LINK {
                copy_to_clipboard(hwnd, &path);
            } else if is_file {
                open_in_explorer_select(&path); // документ: Explorer с выделением файла
            } else {
                let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
                ShellExecuteW(None, w!("open"), PCWSTR(wide.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
            }
        }
        return;
    }
    // имя проекта по menu_target
    let project = APP.with(|c| {
        let a = c.borrow();
        let a = a.as_ref()?;
        a.items.get(a.menu_target).map(|it| it.name.clone())
    });
    let Some(project) = project else { return };

    if (ID_COLOR_BASE..ID_COLOR_BASE + PALETTE.len()).contains(&id) {
        APP.with(|c| {
            if let Some(a) = c.borrow_mut().as_mut() {
                a.config.set_color(&project, id - ID_COLOR_BASE);
                a.config.save(hwnd);
            }
        });
        unsafe {
            let _ = InvalidateRect(hwnd, None, BOOL(0));
        }
    } else if id == ID_LABEL {
        let Some((hinst, cur)) = APP.with(|c| {
            let a = c.borrow();
            let a = a.as_ref()?;
            Some((a.hinst, a.config.label(&project)))
        }) else { return };
        if let Some(s) = prompt::prompt_text(hwnd, hinst, &cur) {
            APP.with(|c| {
                if let Some(a) = c.borrow_mut().as_mut() {
                    a.config.set_label(&project, s.trim().to_string());
                    a.config.save(hwnd);
                }
            });
            unsafe {
                let _ = InvalidateRect(hwnd, None, BOOL(0));
            }
        }
    } else if id == ID_LABEL_CLEAR {
        APP.with(|c| {
            if let Some(a) = c.borrow_mut().as_mut() {
                a.config.set_label(&project, String::new());
                a.config.save(hwnd);
            }
        });
        unsafe {
            let _ = InvalidateRect(hwnd, None, BOOL(0));
        }
    }
}

// Положить текст в буфер обмена (CF_UNICODETEXT). При успехе владение hmem уходит буферу.
unsafe fn copy_to_clipboard(hwnd: HWND, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let Ok(hmem) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) else {
        return;
    };
    let dst = GlobalLock(hmem);
    if dst.is_null() {
        let _ = GlobalFree(hmem); // владение буферу не передано -> освобождаем (D-03)
        return;
    }
    std::ptr::copy_nonoverlapping(wide.as_ptr(), dst as *mut u16, wide.len());
    let _ = GlobalUnlock(hmem);
    if OpenClipboard(hwnd).is_ok() {
        let _ = EmptyClipboard();
        // владение hmem уходит буферу ТОЛЬКО при успешном SetClipboardData; иначе освобождаем (D-03)
        if SetClipboardData(CF_UNICODETEXT, HANDLE(hmem.0)).is_err() {
            let _ = GlobalFree(hmem);
        }
        let _ = CloseClipboard();
    } else {
        let _ = GlobalFree(hmem); // буфер обмена не открылся -> hmem ничей (D-03)
    }
}

// Открыть Проводник с выделением файла: explorer.exe /select,"<path>".
// FPF D-20: карта путей документов освежается фоном раз в ~3 мин, поэтому путь может быть
// протухшим (файл удалили/переместили). Без проверки Проводник открывал пустое окно «Библиотеки»
// без объяснений — вместо этого падаем на папку-родителя, а если и её нет, честно ничего не делаем.
// FPF D-21: путь подставляется в командную строку в кавычках, поэтому `"` внутри пути разорвала бы
// аргумент. В именах файлов Windows кавычка запрещена, но путь приходит из .lnk/индекса, не от ФС.
unsafe fn open_in_explorer_select(path: &str) {
    if path.contains('"') {
        crate::voice::vlog(&format!("open_in_explorer_select: кавычка в пути, отказ ({path})"));
        return;
    }
    let p = std::path::Path::new(path);
    let file: Vec<u16> = "explorer.exe".encode_utf16().chain(std::iter::once(0)).collect();
    let params = if p.exists() {
        format!("/select,\"{path}\"")
    } else {
        // файла нет: открыть ближайшую существующую папку выше по дереву
        let Some(dir) = p.ancestors().skip(1).find(|a| a.exists()) else {
            crate::voice::vlog(&format!("open_in_explorer_select: путь не существует ({path})"));
            return;
        };
        format!("\"{}\"", dir.display())
    };
    let params: Vec<u16> = params.encode_utf16().chain(std::iter::once(0)).collect();
    ShellExecuteW(None, w!("open"), PCWSTR(file.as_ptr()), PCWSTR(params.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL);
}
