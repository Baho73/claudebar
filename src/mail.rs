// FILE: src/mail.rs
// VERSION: 1.2.0
// START_MODULE_CONTRACT
//   PURPOSE: Индикатор входящих: чтение сигналов внешнего роутера почты/мессенджеров о неразобранных письмах по проектам.
//   SCOPE: скан %APPDATA%\claudebar\mail\*.mail (по файлу на проект), разбор ключ=значение, матч со строкой панели, человеческие названия источников и текст подсказки.
//   DEPENDS: M-SIGNAL (sess_matches_row — готовый матч cwd со строкой, включая вложенные папки)
//   EFFECTS: своё: %APPDATA%\claudebar\mail-icons\ — каталог логотипов источников, создаётся пустым (файлы кладёт пользователь); чужое: %APPDATA%\claudebar\mail\ создаём, если роутер ещё не стартовал, но НИЧЕГО в нём не пишем и не удаляем, письма в <проект>\.inbox\ — только чтение и только по клику; владелец обоих — роутер (D:\Python\mail-mcp)
//   REVERT: удалить mail-icons (значки вернутся к запасным цветным маркам); каталог сигналов и письма не наши — их чистит роутер
//   LINKS: M-MAIL
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
//   NOTE: Владелец жизненного цикла сигналов — роутер (D:\Python\mail-mcp). ClaudeBar только ЧИТАЕТ:
//         не удаляет файлы, не ходит в .inbox проектов и не заводит TTL. Файл существует ровно пока
//         есть неразобранное, поэтому «протухания» не бывает (в отличие от .busy в M-SIGNAL).
//         Неразобранным письмо считается, пока в его ПЕРВОЙ строке стоит флаг, который роутер
//         кладёт при доставке. В самой строке написано, что делать: прочитал — удали её. Снимает
//         флаг тот, кто прочитал (обычно агент в этой папке), значок гаснет сам. Письма остаются
//         в .inbox, никуда не переносятся.
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   Mail          - сигнал по одному проекту: cwd, count (всего неразобранного), sources (источник -> сколько)
//   mail_dir      - %APPDATA%\claudebar\mail\ (создаётся при отсутствии)
//   parse_mail    - чистое: текст файла -> Option<Mail>; неизвестные ключи игнорируются
//   parse_sources - чистое: "mailru:6,yandex:1" -> [(mailru,6),(yandex,1)]
//   list          - скан каталога -> Vec<Mail> (порядок стабилен: по cwd)
//   mails_for_row - ВСЕ сигналы строки, включая вложенные проекты (у них своих строк нет)
//   merge_mails   - чистое: свести сигналы в один (сумма писем, объединённые источники)
//   latest_in     - самое свежее письмо среди проектов строки (по дате в имени)
//   inbox_dir / latest_item / pick_latest - папка входящих и свежее письмо одного проекта
//   source_label  - чистое: ключ источника -> человеческое название («mailru» -> «Mail.ru»)
//   tooltip_text  - чистое: Mail -> «7 новых: Mail.ru 6, Яндекс 1»
//   cycle_pick    - чистое: выбор источника для показа по номеру такта (один значок, циклится)
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.2.0 - fix: строка-родитель (ConstructMan) собирает письма ВСЕХ вложенных проектов (mails_for_row + merge_mails), а не первое совпадение — счётчик врал, клик открывал письмо случайного проекта. latest_in берёт самое свежее среди них. Матч использует имя проекта из сигнала, а не имя строки (без пути подходил любой сигнал).
//   v1.1.0 - клик по значку: inbox_dir/latest_item + чистая pick_latest (свежее письмо по ДАТЕ В ИМЕНИ — роутер переписывает файлы пачкой, mtime у всех одинаковый); mail_for_path для меню. Каталог .inbox читается только по клику, не на тике.
//   v1.0.0 - M-MAIL: чтение сигналов роутера входящих (ТЗ docs/TZ-mail-badge.md), матч со строками панели, подписи источников.
// END_CHANGE_SUMMARY

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::UI::WindowsAndMessaging::{LoadImageW, HICON, IMAGE_ICON, LR_LOADFROMFILE};

// Иконку грузим 16x16 (рисуем меньше — DrawIconEx масштабирует).
const ICON_PX: i32 = 16;

thread_local! {
    // Кэш логотипов источников на UI-потоке: ключ -> HICON или None («файла нет»).
    static ICON_CACHE: RefCell<HashMap<String, Option<HICON>>> = RefCell::new(HashMap::new());
}

// Сигнал по одному проекту: столько неразобранного лежит в его .inbox.
#[derive(Clone, Debug, PartialEq)]
pub struct Mail {
    pub cwd: String,                 // папка проекта (как записал роутер)
    pub count: u32,                  // сколько неразобранного всего
    pub sources: Vec<(String, u32)>, // разбивка: ключ источника -> количество
}

// START_CONTRACT: mail_dir
//   PURPOSE: Каталог сигналов входящих; создаётся, если его ещё нет.
//   INPUTS: {}
//   OUTPUTS: { PathBuf - %APPDATA%\claudebar\mail }
//   SIDE_EFFECTS: create_dir_all (роутер и панель могут стартовать в любом порядке)
// END_CONTRACT: mail_dir
pub fn mail_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
    let dir = base.join("claudebar").join("mail");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

// START_CONTRACT: parse_sources
//   PURPOSE: Разобрать поле sources: «ключ:количество» через запятую.
//   INPUTS: { s: &str - например "mailru:6,yandex:1" }
//   OUTPUTS: { Vec<(String, u32)> - порядок как в файле; битые пары пропускаются }
//   SIDE_EFFECTS: none
// END_CONTRACT: parse_sources
pub fn parse_sources(s: &str) -> Vec<(String, u32)> {
    s.split(',')
        .filter_map(|p| {
            let (k, v) = p.trim().split_once(':')?;
            let k = k.trim();
            if k.is_empty() {
                return None;
            }
            Some((k.to_lowercase(), v.trim().parse().ok()?))
        })
        .collect()
}

// START_CONTRACT: parse_mail
//   PURPOSE: Разобрать файл сигнала (построчно ключ=значение) в Mail.
//   INPUTS: { text: &str - содержимое <hash>.mail }
//   OUTPUTS: { Option<Mail> - None если нет cwd или count=0 (нечего показывать) }
//   SIDE_EFFECTS: none
//   NOTE: К незнакомым ключам терпимы намеренно — роутер вправе дописывать поля, панель не должна
//         ломаться от расширения формата.
// END_CONTRACT: parse_mail
pub fn parse_mail(text: &str) -> Option<Mail> {
    let mut cwd = String::new();
    let mut count = 0u32;
    let mut sources = Vec::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "cwd" => cwd = v.trim().to_string(),
            "count" => count = v.trim().parse().unwrap_or(0),
            "sources" => sources = parse_sources(v),
            _ => {} // незнакомый ключ — не повод отбрасывать сигнал
        }
    }
    if cwd.is_empty() || count == 0 {
        return None;
    }
    Some(Mail { cwd, count, sources })
}

// START_CONTRACT: list
//   PURPOSE: Прочитать все сигналы входящих из каталога.
//   INPUTS: {}
//   OUTPUTS: { Vec<Mail> - отсортированы по cwd, чтобы порядок не плясал между тиками }
//   SIDE_EFFECTS: чтение каталога и файлов (раз в секунду из общего тика)
//   LINKS: M-MAIL, M-MAIN (refresh_items)
// END_CONTRACT: list
pub fn list() -> Vec<Mail> {
    let Ok(rd) = std::fs::read_dir(mail_dir()) else { return Vec::new() };
    let mut out: Vec<Mail> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("mail")))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| parse_mail(&t))
        .collect();
    out.sort_by(|a, b| a.cwd.cmp(&b.cwd));
    out
}

// START_CONTRACT: mails_for_row
//   PURPOSE: ВСЕ сигналы, относящиеся к строке панели (включая вложенные проекты).
//   INPUTS: { mails: &[Mail]; row_path: Option<&str>; row_name: &str }
//   OUTPUTS: { Vec<&Mail> - порядок как в mails (по cwd); пусто, если совпадений нет }
//   SIDE_EFFECTS: none
//   NOTE: Строка «ConstructMan» — одно окно, а письма лежат в ConstructMan\KSK, \Shale, \VINODEL,
//         своих строк у них нет. Брать первое совпадение нельзя: счётчик показывал бы письма
//         одного вложенного проекта, а клик открывал бы письмо случайного из них.
// END_CONTRACT: mails_for_row
pub fn mails_for_row<'a>(mails: &'a [Mail], row_path: Option<&str>, row_name: &str) -> Vec<&'a Mail> {
    mails
        .iter()
        .filter(|m| {
            // key — имя проекта ИЗ СИГНАЛА (как в M-SIGNAL у сессии), а не имя строки: иначе при
            // отсутствии пути у строки сравнивалось бы имя само с собой и подходил бы любой сигнал.
            let key = m.cwd.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or(&m.cwd).to_lowercase();
            crate::signal::sess_matches_row(&m.cwd, &key, row_path, row_name)
        })
        .collect()
}

// START_CONTRACT: merge_mails
//   PURPOSE: Чистое — свести несколько сигналов в один: сумма писем и объединённые источники.
//   INPUTS: { ms: &[&Mail] }
//   OUTPUTS: { Option<Mail> - cwd первого, count = сумма, sources сложены по ключу }
//   SIDE_EFFECTS: none
//   NOTE: Источники сортируются по убыванию количества, затем по имени — порядок значка стабилен
//         между тиками, иначе иконки прыгали бы при каждом пересчёте.
// END_CONTRACT: merge_mails
pub fn merge_mails(ms: &[&Mail]) -> Option<Mail> {
    let first = ms.first()?;
    if ms.len() == 1 {
        return Some((*first).clone());
    }
    let mut acc: Vec<(String, u32)> = Vec::new();
    let mut count = 0u32;
    for m in ms {
        count += m.count;
        for (k, n) in &m.sources {
            match acc.iter_mut().find(|(ak, _)| ak == k) {
                Some((_, an)) => *an += n,
                None => acc.push((k.clone(), *n)),
            }
        }
    }
    acc.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Some(Mail { cwd: first.cwd.clone(), count, sources: acc })
}

// START_CONTRACT: latest_in
//   PURPOSE: Самое свежее письмо среди НЕСКОЛЬКИХ проектов строки (по дате в имени файла).
//   INPUTS: { ms: &[&Mail] }
//   OUTPUTS: { Option<PathBuf> }
//   SIDE_EFFECTS: чтение каталогов .inbox — только по клику, не на тике
// END_CONTRACT: latest_in
pub fn latest_in(ms: &[&Mail]) -> Option<PathBuf> {
    ms.iter().filter_map(|m| latest_item(&m.cwd)).max_by(|a, b| a.file_name().cmp(&b.file_name()))
}

// START_CONTRACT: source_label
//   PURPOSE: Человеческое название источника для подсказки (ключи роутера — машинные).
//   INPUTS: { key: &str }
//   OUTPUTS: { String - «Mail.ru», «Яндекс», …; незнакомый ключ возвращается как есть }
//   SIDE_EFFECTS: none
// END_CONTRACT: source_label
pub fn source_label(key: &str) -> String {
    match key {
        "mailru" => "Mail.ru",
        "yandex" => "Яндекс",
        "gmail" => "Gmail",
        "telegram" => "Telegram",
        "max" => "MAX",
        "sca_order" => "СК: заказы",
        "sca_info" => "СК: инфо",
        "sca_admin" => "СК: админ",
        "custombot" => "Бот",
        other => return other.to_string(), // список пополняемый — незнакомое не прячем
    }
    .to_string()
}

// START_CONTRACT: tooltip_text
//   PURPOSE: Текст подсказки по значку входящих.
//   INPUTS: { m: &Mail }
//   OUTPUTS: { String - «7 новых: Mail.ru 6, Яндекс 1»; без разбивки — «7 новых» }
//   SIDE_EFFECTS: none
// END_CONTRACT: tooltip_text
pub fn tooltip_text(m: &Mail) -> String {
    if m.sources.is_empty() {
        return format!("{} новых", m.count);
    }
    let parts: Vec<String> = m.sources.iter().map(|(k, n)| format!("{} {}", source_label(k), n)).collect();
    format!("{} новых: {}", m.count, parts.join(", "))
}

// START_CONTRACT: cycle_pick
//   PURPOSE: Какой источник показывать на этом такте: значок ОДИН и циклится по источникам.
//   INPUTS: { sources_len: usize; tick: u32 - счётчик секундных тактов }
//   OUTPUTS: { Option<usize> - индекс источника; None если источников нет }
//   SIDE_EFFECTS: none
//   NOTE: При единственном источнике всегда 0 — значок статичен, без мигания (требование ТЗ).
// END_CONTRACT: cycle_pick
pub fn cycle_pick(sources_len: usize, tick: u32) -> Option<usize> {
    if sources_len == 0 {
        return None;
    }
    Some((tick as usize) % sources_len)
}

// START_CONTRACT: icons_dir
//   PURPOSE: Каталог иконок источников: %APPDATA%\claudebar\mail-icons\<ключ>.ico
//   INPUTS: {}
//   OUTPUTS: { PathBuf }
//   SIDE_EFFECTS: create_dir_all (чтобы пользователю было куда класть файлы)
//   NOTE: Иконки лежат файлами, а не зашиты в бинарь: список источников роутера пополняемый
//         (новый ключ -> просто положить <ключ>.ico, код не трогаем), и чужие логотипы не едут
//         в публичный репозиторий. Нет файла -> M-RENDER рисует запасную марку.
// END_CONTRACT: icons_dir
pub fn icons_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
    let dir = base.join("claudebar").join("mail-icons");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

// START_CONTRACT: safe_key
//   PURPOSE: Проверить, что ключ источника пригоден как имя файла (граница доверия).
//   INPUTS: { key: &str - пришёл из файла, который пишет ВНЕШНИЙ роутер }
//   OUTPUTS: { bool - только [a-z0-9_-], непусто, не длиннее 32 }
//   SIDE_EFFECTS: none
//   NOTE: Без проверки ключ с ".." увёл бы чтение за пределы каталога иконок.
// END_CONTRACT: safe_key
pub fn safe_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 32
        && key.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
}

// START_CONTRACT: source_icon
//   PURPOSE: HICON логотипа источника из <ключ>.ico; None -> вызывающий рисует запасную марку.
//   INPUTS: { key: &str - ключ источника из сигнала }
//   OUTPUTS: { Option<HICON> - кэшируется по ключу, в т.ч. отрицательный результат }
//   SIDE_EFFECTS: чтение файла при первом обращении (дальше кэш на UI-потоке)
//   LINKS: M-MAIL, M-RENDER (рисует), M-ICON (тот же приём: HICON + DrawIconEx)
// END_CONTRACT: source_icon
pub fn source_icon(key: &str) -> Option<HICON> {
    if !safe_key(key) {
        return None;
    }
    ICON_CACHE.with(|c| {
        if let Some(hit) = c.borrow().get(key) {
            return *hit; // кэшируем и «файла нет», чтобы не дёргать диск каждый кадр
        }
        let path = icons_dir().join(format!("{key}.ico"));
        let icon = load_ico(&path);
        c.borrow_mut().insert(key.to_string(), icon);
        icon
    })
}

// Загрузить .ico с диска (Windows читает формат нативно — декодер не нужен).
fn load_ico(path: &std::path::Path) -> Option<HICON> {
    if !path.is_file() {
        return None;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let h = unsafe {
        LoadImageW(None, PCWSTR(wide.as_ptr()), IMAGE_ICON, ICON_PX, ICON_PX, LR_LOADFROMFILE).ok()?
    };
    if h.0.is_null() {
        return None;
    }
    Some(HICON(h.0))
}

// START_CONTRACT: fallback_color
//   PURPOSE: Цвет запасной марки источника, когда файла <ключ>.ico нет.
//   INPUTS: { key: &str }
//   OUTPUTS: { (u8,u8,u8) - фирменный цвет сервиса; незнакомый -> серый }
//   SIDE_EFFECTS: none
//   NOTE: Это НЕ логотип, а различимая метка: пока пользователь не положил .ico, источники
//         всё равно отличаются друг от друга цветом, и значок не пропадает совсем.
// END_CONTRACT: fallback_color
pub fn fallback_color(key: &str) -> (u8, u8, u8) {
    match key {
        "mailru" => (0, 91, 255),
        "yandex" => (252, 60, 45),
        "gmail" => (234, 67, 53),
        "telegram" => (42, 171, 238),
        "max" => (138, 92, 246),
        k if k.starts_with("sca_") => (245, 158, 11),
        "custombot" => (16, 185, 129),
        _ => (150, 165, 200),
    }
}

// START_CONTRACT: inbox_dir
//   PURPOSE: Папка входящих проекта: <cwd>\.inbox
//   INPUTS: { cwd: &str }
//   OUTPUTS: { PathBuf }
//   SIDE_EFFECTS: none (не создаём — каталог заводит роутер)
// END_CONTRACT: inbox_dir
pub fn inbox_dir(cwd: &str) -> PathBuf {
    PathBuf::from(cwd).join(".inbox")
}

// START_CONTRACT: pick_latest
//   PURPOSE: Чистое — выбрать самое свежее входящее по ИМЕНИ файла.
//   INPUTS: { names: &[String] - имена файлов в .inbox }
//   OUTPUTS: { Option<String> - максимальное имя среди *.md }
//   SIDE_EFFECTS: none
//   NOTE: Именно по имени, а не по mtime: роутер переписывает файлы пачкой, и время у всех
//         одинаковое. Имя начинается с даты «ГГГГ-ММ-ДД-…», поэтому лексикографический максимум
//         и есть самое свежее письмо.
// END_CONTRACT: pick_latest
pub fn pick_latest(names: &[String]) -> Option<String> {
    names.iter().filter(|n| n.to_lowercase().ends_with(".md")).max().cloned()
}

// START_CONTRACT: latest_item
//   PURPOSE: Путь к самому свежему неразобранному письму проекта (для клика по значку).
//   INPUTS: { cwd: &str - папка проекта из сигнала }
//   OUTPUTS: { Option<PathBuf> - <cwd>\.inbox\<самое свежее>.md }
//   SIDE_EFFECTS: чтение каталога .inbox — ТОЛЬКО по клику, не на тике (запрет из ТЗ)
//   LINKS: M-MAIL, M-MAIN (клик по значку)
// END_CONTRACT: latest_item
pub fn latest_item(cwd: &str) -> Option<PathBuf> {
    let dir = inbox_dir(cwd);
    let names: Vec<String> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    pick_latest(&names).map(|n| dir.join(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mail_reads_router_format() {
        let m = parse_mail("cwd=D:\\Python\\_inbox\\бухгалтерия\ncount=7\nsources=mailru:6,yandex:1\n").unwrap();
        assert_eq!(m.cwd, "D:\\Python\\_inbox\\бухгалтерия");
        assert_eq!(m.count, 7);
        assert_eq!(m.sources, vec![("mailru".to_string(), 6), ("yandex".to_string(), 1)]);
        // незнакомый ключ не ломает разбор (формат роутера вправе расширяться)
        assert!(parse_mail("cwd=D:\\x\ncount=1\nnewfield=что-то\n").is_some());
        // нет cwd или нечего показывать -> сигнала нет
        assert!(parse_mail("count=3\nsources=max:3").is_none());
        assert!(parse_mail("cwd=D:\\x\ncount=0").is_none());
        assert!(parse_mail("мусор").is_none());
    }

    #[test]
    fn parse_sources_tolerates_junk() {
        assert_eq!(parse_sources("max:4"), vec![("max".to_string(), 4)]);
        assert_eq!(
            parse_sources(" telegram : 2 , sca_order:2 "),
            vec![("telegram".to_string(), 2), ("sca_order".to_string(), 2)]
        );
        assert!(parse_sources("").is_empty());
        assert_eq!(parse_sources("битое,max:1"), vec![("max".to_string(), 1)]); // пара без ':' пропущена
    }

    #[test]
    fn mail_for_row_matches_full_path_and_subfolders() {
        let mails = vec![
            Mail { cwd: "D:\\Python\\ConstructMan\\Shale".into(), count: 4, sources: vec![("max".into(), 4)] },
            Mail { cwd: "D:\\Python\\margo-ai".into(), count: 2, sources: vec![("telegram".into(), 2)] },
        ];
        // строка с полным путём: точное совпадение
        assert_eq!(mails_for_row(&mails, Some("D:\\Python\\margo-ai"), "margo-ai")[0].count, 2);
        // сигнал во вложенной папке засчитывается строке проекта-родителя (механика M-SIGNAL)
        assert_eq!(mails_for_row(&mails, Some("D:\\Python\\ConstructMan"), "ConstructMan")[0].count, 4);
        // регистр и слэши не мешают
        assert!(mails_for_row(&mails, Some("d:/python/margo-ai"), "margo-ai").len() == 1);
        // одноимённый проект в другом месте не ловится (D-06)
        assert!(mails_for_row(&mails, Some("D:\\Other\\margo-ai"), "margo-ai").is_empty());
        // без пути падаем на имя строки
        assert!(mails_for_row(&mails, None, "margo-ai").len() == 1);
    }

    #[test]
    fn tooltip_and_labels_are_human() {
        let m = Mail { cwd: "x".into(), count: 7, sources: vec![("mailru".into(), 6), ("yandex".into(), 1)] };
        assert_eq!(tooltip_text(&m), "7 новых: Mail.ru 6, Яндекс 1");
        let bare = Mail { cwd: "x".into(), count: 3, sources: vec![] };
        assert_eq!(tooltip_text(&bare), "3 новых");
        assert_eq!(source_label("неизвестный_ключ"), "неизвестный_ключ"); // пополняемый список
    }

    #[test]
    fn pick_latest_uses_name_not_mtime() {
        // роутер переписывает файлы пачкой -> mtime одинаковый; дата в имени и есть порядок
        let names: Vec<String> = vec![
            "2026-08-16-АО-Почта-России.md".into(),
            "2026-08-22-новые-в-StroyControl.md".into(),
            "2026-07-22-Пора-оплатить-налоги.md".into(),
            "done".into(),          // подпапка разобранного — не письмо
            "заметка.txt".into(),   // не .md
        ];
        assert_eq!(pick_latest(&names).as_deref(), Some("2026-08-22-новые-в-StroyControl.md"));
        assert_eq!(pick_latest(&[]), None);
        assert_eq!(pick_latest(&["readme.txt".to_string()]), None);
    }

    #[test]
    fn safe_key_blocks_path_tricks() {
        // ключ приходит из файла внешнего роутера -> используется как имя файла
        assert!(safe_key("mailru") && safe_key("sca_order") && safe_key("max"));
        assert!(!safe_key(""));
        assert!(!safe_key("..")); // выход из каталога
        assert!(!safe_key("../../windows/system32/x"));
        assert!(!safe_key("Mailru")); // ключи роутера в нижнем регистре
        assert!(!safe_key("a b"));
        assert!(!safe_key(&"x".repeat(33)));
        // незнакомый источник всё равно получает различимый цвет
        assert_ne!(fallback_color("telegram"), fallback_color("mailru"));
        assert_eq!(fallback_color("sca_info"), fallback_color("sca_order"));
    }

    #[test]
    fn cycle_pick_is_static_for_single_source() {
        assert_eq!(cycle_pick(0, 5), None);
        assert_eq!(cycle_pick(1, 0), Some(0));
        assert_eq!(cycle_pick(1, 999), Some(0)); // один источник -> без мигания
        assert_eq!(cycle_pick(2, 0), Some(0));
        assert_eq!(cycle_pick(2, 1), Some(1));
        assert_eq!(cycle_pick(2, 2), Some(0)); // цикл
    }
}
