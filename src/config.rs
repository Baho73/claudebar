// FILE: src/config.rs
// VERSION: 1.12.0
// START_MODULE_CONTRACT
//   PURPOSE: Конфигурация: приложения (по процессу и классу окна), настройки проектов (цвет, метка), свёрнутость секций, «показать все» недавних, позиция, шрифт панели.
//   SCOPE: палитра, авто-цвет, AppDef (proc/proc_alts/class)/NameMode (вкл. Whole), дефолтный набор приложений (редакторы, Office, терминалы, Проводник), свёрнутость секций, раскрытие/showall недавних, шрифт (font_face/font_size/font_weight), парсинг/сериализация ini.
//   DEPENDS: none (Win32 только для чтения позиции окна при сохранении)
//   LINKS: M-CONFIG
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   Config        - корень конфигурации: apps, projects, collapsed, recent_expanded, recent_showall, section_order, window_order, font_face, font_size, pos, cfg_path
//   AppDef        - приложение: proc + proc_alts, class (фильтр окна), имя блока, правило имени, расширения
//   NameMode      - извлечение имени: Project{suffix} | Document | DocumentLast | Whole
//   set_font / font_face / font_size / font_weight - шрифт панели (ключ font=face⇥size⇥weight), дефолт Iosevka Fixed/16/600
//   visible_start_pos - стартовая позиция окна с учётом конфигурации мониторов: дефолт, если saved вне виртуального экрана
//   search_db / search_cmd / search_port - конфиг поиска по чатам: путь clfind.db, команда dense-демона, порт (ключи searchdb=/searchcmd=/searchport=) — Phase-12 (dormant: dense отложен)
//   chats_db / files_db / projects_root - Phase-13: свои FTS5-базы (BM25 на Rust) и корень транскриптов (ключи chatsdb=/filesdb=/projectsroot=)
//   ProjConf      - настройки проекта: индекс цвета (-1 = авто), метка
//   PALETTE       - палитра из 8 цветов
//   auto_color    - детерминированный цвет по имени
//   default_apps  - встроенный набор приложений
//   section_index_order / window_rank - применение ручного порядка
//   move_section / move_window         - перестановка при drag-reorder
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.12.0 - Phase-13 доводка: персист scope «+Файлы» (search_files, ключ searchfiles=).
//   v1.11.0 - Phase-13 Ф-A step-2: свои базы (chats_db/files_db, дефолт %APPDATA%\claudebar) + projects_root (дефолт %USERPROFILE%\.claude\projects); ключи chatsdb=/filesdb=/projectsroot=. search_* остаются dormant (dense отложен).
//   v1.10.0 - Phase-12 Step 1: конфиг поиска (search_db/search_cmd/search_port; ключи searchdb=/searchcmd=/searchport=) для M-SEARCH/M-SDAEMON.
//   v1.9.0 - fix(grace-fix): visible_start_pos — стартовая позиция с учётом мониторов. После смены конфигурации мониторов сохранённая pos= оказывалась вне виртуального экрана, окно создавалось за экраном (видно лишь в панели задач). M-MAIN теперь клампит через SM_*VIRTUALSCREEN.
//   v1.8.0 - fix(grace-fix): вес шрифта (font_weight, 3-е поле font=); set_font(face,size,weight). Без веса панель была всегда 600 (жирная) и диалог не предзаполнял стиль.
//   v1.7.0 - Phase-10 Step 1: AppDef + proc_alts/class; NameMode::Whole; default_apps += терминалы (Windows Terminal, cmd, PowerShell, Git Bash) и Проводник.
//   v1.6.0 - Phase-9 Step 1: шрифт панели (font_face/font_size, ключ font=, деф. Iosevka Fixed/16); set_font; round-trip.
//   v1.5.0 - Phase-8 Step 1: ручной порядок секций (os=) и окон в секции (o=); parse_ini -> struct ParsedIni.
//   v1.4.0 - Phase-7 Step 2: recent_showall — «показать все» недавних сверх 6 с персистом (ra=).
//   v1.3.0 - Phase-3 Step 2: recent_expanded — состояние раскрытия под-блока «недавние» с персистом (re=).
//   v1.2.1 - fix: NameMode::DocumentLast для MS Project (заголовок "App - Файл"), показываем имя файла.
//   v1.2.0 - Phase-2 Step 2: свёрнутость секций (collapsed) с персистом в ini.
//   v1.1.0 - Phase-2 Step 1: AppDef/NameMode, приложения по процессу.
//   v1.0.0 - Выделено из монолита (Phase-1, Step 1).
// END_CHANGE_SUMMARY

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

// Шрифт панели по умолчанию (моноширинный, хорошо читается в списках/таблицах).
pub const DEFAULT_FONT: &str = "Iosevka Fixed";
pub const DEFAULT_FONT_SIZE: i32 = 16;
pub const DEFAULT_FONT_WEIGHT: i32 = 600; // полужирный — текущий вид заголовков/имён по умолчанию

// Поиск по чатам (Phase-12): команда запуска dense-демона clfind и его порт.
pub const DEFAULT_SEARCH_CMD: &str = "D:\\Python\\clfind\\.venv\\Scripts\\pythonw.exe -m clfind.cli serve";
pub const DEFAULT_SEARCH_PORT: u16 = 8799;

// Путь к индексу clfind по умолчанию: %USERPROFILE%\.clfind\clfind.db.
fn default_search_db() -> String {
    let base = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
    base.join(".clfind").join("clfind.db").to_string_lossy().into_owned()
}

// Phase-13: свои базы (BM25 на Rust) в %APPDATA%\claudebar и корень транскриптов.
fn appdata_db(name: &str) -> String {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
    base.join("claudebar").join(name).to_string_lossy().into_owned()
}
fn default_chats_db() -> String {
    appdata_db("claudebar_chats.db")
}
fn default_files_db() -> String {
    appdata_db("claudebar_files.db")
}
// Корень транскриптов Claude Code: %USERPROFILE%\.claude\projects.
fn default_projects_root() -> String {
    let base = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
    base.join(".claude").join("projects").to_string_lossy().into_owned()
}

pub const PALETTE: [(&str, u8, u8, u8); 8] = [
    ("Синий", 0x5B, 0x8F, 0xF9),
    ("Зелёный", 0x61, 0xD4, 0xA6),
    ("Жёлтый", 0xF6, 0xBD, 0x16),
    ("Красный", 0xE8, 0x68, 0x4A),
    ("Фиолетовый", 0xB3, 0x7F, 0xEB),
    ("Голубой", 0x6D, 0xC8, 0xEC),
    ("Розовый", 0xFF, 0x99, 0xC3),
    ("Серый", 0x9A, 0xA7, 0xB1),
];

#[derive(Clone, Default)]
pub struct ProjConf {
    pub color: i32, // -1 = авто по имени
    pub label: String,
}

#[derive(Clone)]
pub enum NameMode {
    // Проект редактора: отбросить суффикс приложения, взять последний сегмент по " - ".
    Project { suffix: String },
    // Имя документа в первом сегменте по " - " (Word/Excel: "Файл - App").
    Document,
    // Имя документа в последнем сегменте по " - " (MS Project: "App - Файл").
    DocumentLast,
    // Весь заголовок как имя (терминалы, папки Проводника).
    Whole,
}

#[derive(Clone)]
pub struct AppDef {
    pub proc: String, // основной процесс (exe, нижний регистр), напр. "code.exe"
    pub proc_alts: Vec<String>, // дополнительные процессы (напр. "pwsh.exe" для PowerShell)
    pub class: Option<String>, // требуемый класс окна (None = любой); напр. "CabinetWClass"
    pub block: String, // отображаемое имя секции
    pub mode: NameMode, // правило извлечения имени
    pub exts: Vec<String>, // расширения недавних документов (Office)
    pub editor_storage: Option<String>, // для редакторов: подпапка в %APPDATA% (Code/Cursor) — недавние проекты
}

pub struct Config {
    pub apps: Vec<AppDef>,
    pub projects: HashMap<String, ProjConf>,
    pub proj_numbers: HashMap<String, i32>, // полный путь проекта -> стабильный № дубля (ключ pn=) — Phase-15
    pub collapsed: HashSet<String>, // имена свёрнутых секций (block)
    pub recent_expanded: HashSet<String>, // секции с раскрытым под-блоком «недавние»
    pub recent_showall: HashSet<String>, // секции с раскрытым полным списком недавних (сверх 6)
    pub section_order: Vec<String>, // ручной порядок секций (block); пустой = по умолчанию
    pub window_order: HashMap<String, Vec<String>>, // block -> ручной порядок имён окон
    pub font_face: String, // гарнитура шрифта панели (ключ font=)
    pub font_size: i32, // базовый кегль (px); мелкий шрифт = font_size-3
    pub font_weight: i32, // насыщенность основного шрифта (100..900); мелкий = min(weight, 400)
    pub pos: Option<(i32, i32)>,
    pub search_db: String, // путь к индексу clfind.db (нативный BM25 через rusqlite) — Phase-12
    pub search_cmd: String, // команда запуска dense-демона (clfind serve) — Phase-12
    pub search_port: u16, // порт HTTP-демона dense — Phase-12
    pub chats_db: String, // своя FTS5-база чатов (нативный BM25 на Rust) — Phase-13
    pub files_db: String, // своя FTS5-база файлов (Ф-C) — Phase-13
    pub projects_root: String, // корень транскриптов Claude Code для индексации — Phase-13
    pub search_files: bool, // scope «+Файлы»: искать и в claudebar_files.db (ключ searchfiles=) — Phase-13
    pub cfg_path: PathBuf,
}

// перестановка элемента списка с позиции from на позицию to (вставка)
fn reorder_vec(items: &mut Vec<String>, from: usize, to: usize) {
    if from >= items.len() || to >= items.len() || from == to {
        return;
    }
    let it = items.remove(from);
    items.insert(to, it);
}

pub fn auto_color(name: &str) -> usize {
    let h = name
        .bytes()
        .fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    (h % PALETTE.len() as u32) as usize
}

// Минимальная видимая часть окна (px), при которой панель ещё можно увидеть и схватить.
const MIN_VISIBLE_W: i32 = 80;
const MIN_VISIBLE_H: i32 = 24;

// START_CONTRACT: visible_start_pos
//   PURPOSE: Выбрать стартовую позицию панели с учётом текущей конфигурации мониторов.
//   INPUTS: { saved: Option<(i32,i32)>, default: (i32,i32), win_w, win_h, virtual screen rect (vx,vy,vw,vh) }
//   OUTPUTS: { (i32, i32) — saved, если окно достаточно видно на объединённом экране; иначе default }
//   SIDE_EFFECTS: none (чистая арифметика)
// END_CONTRACT: visible_start_pos
/// Если сохранённая позиция оставляет слишком мало видимой площади окна на
/// объединённом прямоугольнике всех мониторов (virtual screen) — монитор
/// отключён или переставлен — вернуть `default` (на первичном экране).
pub fn visible_start_pos(
    saved: Option<(i32, i32)>,
    default: (i32, i32),
    win_w: i32,
    win_h: i32,
    vx: i32,
    vy: i32,
    vw: i32,
    vh: i32,
) -> (i32, i32) {
    let (x, y) = match saved {
        Some(p) => p,
        None => return default,
    };
    // пересечение окна с объединённым прямоугольником всех мониторов
    let vis_w = (x + win_w).min(vx + vw) - x.max(vx);
    let vis_h = (y + win_h).min(vy + vh) - y.max(vy);
    if vis_w >= MIN_VISIBLE_W && vis_h >= MIN_VISIBLE_H {
        (x, y)
    } else {
        default
    }
}

// START_CONTRACT: default_apps
//   PURPOSE: Встроенный набор отслеживаемых приложений с правилами имени и расширениями.
//   INPUTS: {}
//   OUTPUTS: { Vec<AppDef> }
//   SIDE_EFFECTS: none
// END_CONTRACT: default_apps
pub fn default_apps() -> Vec<AppDef> {
    fn app(proc: &str, block: &str, mode: NameMode, exts: &[&str], editor: Option<&str>) -> AppDef {
        AppDef {
            proc: proc.to_string(),
            proc_alts: Vec::new(),
            class: None,
            block: block.to_string(),
            mode,
            exts: exts.iter().map(|s| s.to_string()).collect(),
            editor_storage: editor.map(|s| s.to_string()),
        }
    }
    // Терминал/Проводник: имя = весь заголовок (Whole), без недавних; class — опциональный фильтр окна.
    fn shell(proc: &str, alts: &[&str], class: Option<&str>, block: &str) -> AppDef {
        AppDef {
            proc: proc.to_string(),
            proc_alts: alts.iter().map(|s| s.to_string()).collect(),
            class: class.map(|s| s.to_string()),
            block: block.to_string(),
            mode: NameMode::Whole,
            exts: Vec::new(),
            editor_storage: None,
        }
    }
    vec![
        app("code.exe", "VS Code", NameMode::Project { suffix: " - Visual Studio Code".into() }, &[], Some("Code")),
        app("cursor.exe", "Cursor", NameMode::Project { suffix: " - Cursor".into() }, &[], Some("Cursor")),
        app("winword.exe", "Word", NameMode::Document, &["docx", "doc", "rtf"], None),
        app("excel.exe", "Excel", NameMode::Document, &["xlsx", "xls", "csv"], None),
        app("winproj.exe", "MS Project", NameMode::DocumentLast, &["mpp"], None),
        // Терминалы: cmd и PowerShell в классической консоли различаются по дереву процессов
        // (см. console_client в win_enum); class=ConsoleWindowClass отсекает не-консольные окна.
        shell("windowsterminal.exe", &[], None, "Windows Terminal"),
        shell("cmd.exe", &[], Some("ConsoleWindowClass"), "Командная строка"),
        shell("powershell.exe", &["pwsh.exe"], Some("ConsoleWindowClass"), "PowerShell"),
        shell("mintty.exe", &["bash.exe", "git-bash.exe"], None, "Git Bash"),
        // Проводник: только окна папок (class=CabinetWClass), иначе сюда лезут панель задач и рабочий стол.
        shell("explorer.exe", &[], Some("CabinetWClass"), "Проводник"),
    ]
}

struct ParsedIni {
    projects: HashMap<String, ProjConf>,
    proj_numbers: HashMap<String, i32>,
    collapsed: HashSet<String>,
    recent_expanded: HashSet<String>,
    recent_showall: HashSet<String>,
    section_order: Vec<String>,
    window_order: HashMap<String, Vec<String>>,
    font_face: String,
    font_size: i32,
    font_weight: i32,
    pos: Option<(i32, i32)>,
    search_db: String,
    search_cmd: String,
    search_port: u16,
    chats_db: String,
    files_db: String,
    projects_root: String,
    search_files: bool,
}

fn parse_ini(text: &str) -> ParsedIni {
    let mut projects: HashMap<String, ProjConf> = HashMap::new();
    let mut proj_numbers: HashMap<String, i32> = HashMap::new();
    let mut collapsed: HashSet<String> = HashSet::new();
    let mut recent_expanded: HashSet<String> = HashSet::new();
    let mut recent_showall: HashSet<String> = HashSet::new();
    let mut section_order: Vec<String> = Vec::new();
    let mut window_order: HashMap<String, Vec<String>> = HashMap::new();
    let mut font_face: String = DEFAULT_FONT.to_string();
    let mut font_size: i32 = DEFAULT_FONT_SIZE;
    let mut font_weight: i32 = DEFAULT_FONT_WEIGHT;
    let mut pos: Option<(i32, i32)> = None;
    let mut search_db: String = default_search_db();
    let mut search_cmd: String = DEFAULT_SEARCH_CMD.to_string();
    let mut search_port: u16 = DEFAULT_SEARCH_PORT;
    let mut chats_db: String = default_chats_db();
    let mut files_db: String = default_files_db();
    let mut projects_root: String = default_projects_root();
    let mut search_files = false;
    // START_BLOCK_PARSE_LINES
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("pos=") {
            let mut it = v.split(',');
            if let (Some(x), Some(y)) = (it.next(), it.next()) {
                if let (Ok(x), Ok(y)) = (x.trim().parse(), y.trim().parse()) {
                    pos = Some((x, y));
                }
            }
        } else if let Some(v) = line.strip_prefix("font=") {
            // font=<гарнитура>\t<кегль>\t<вес>; гарнитура может содержать пробелы, но не табы.
            // Вес — необязательное 3-е поле (обратная совместимость со старым 2-польным форматом).
            let mut it = v.splitn(3, '\t');
            if let Some(face) = it.next() {
                if !face.trim().is_empty() {
                    font_face = face.to_string();
                }
            }
            if let Some(sz) = it.next() {
                if let Ok(n) = sz.trim().parse::<i32>() {
                    if (6..=72).contains(&n) {
                        font_size = n;
                    }
                }
            }
            if let Some(wt) = it.next() {
                if let Ok(n) = wt.trim().parse::<i32>() {
                    if (100..=900).contains(&n) {
                        font_weight = n;
                    }
                }
            }
        } else if let Some(v) = line.strip_prefix("searchdb=") {
            if !v.trim().is_empty() {
                search_db = v.to_string();
            }
        } else if let Some(v) = line.strip_prefix("searchcmd=") {
            if !v.trim().is_empty() {
                search_cmd = v.to_string();
            }
        } else if let Some(v) = line.strip_prefix("searchport=") {
            if let Ok(n) = v.trim().parse::<u16>() {
                if n != 0 {
                    search_port = n;
                }
            }
        } else if let Some(v) = line.strip_prefix("chatsdb=") {
            if !v.trim().is_empty() {
                chats_db = v.to_string();
            }
        } else if let Some(v) = line.strip_prefix("filesdb=") {
            if !v.trim().is_empty() {
                files_db = v.to_string();
            }
        } else if let Some(v) = line.strip_prefix("projectsroot=") {
            if !v.trim().is_empty() {
                projects_root = v.to_string();
            }
        } else if let Some(v) = line.strip_prefix("searchfiles=") {
            search_files = v.trim() == "1";
        } else if let Some(v) = line.strip_prefix("os=") {
            section_order = v.split('\t').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
        } else if let Some(v) = line.strip_prefix("o=") {
            let mut it = v.split('\t');
            if let Some(block) = it.next() {
                let names: Vec<String> = it.filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
                if !block.is_empty() && !names.is_empty() {
                    window_order.insert(block.to_string(), names);
                }
            }
        } else if let Some(v) = line.strip_prefix("ra=") {
            if !v.is_empty() {
                recent_showall.insert(v.to_string());
            }
        } else if let Some(v) = line.strip_prefix("re=") {
            if !v.is_empty() {
                recent_expanded.insert(v.to_string());
            }
        } else if let Some(v) = line.strip_prefix("c=") {
            if !v.is_empty() {
                collapsed.insert(v.to_string());
            }
        } else if let Some(v) = line.strip_prefix("pn=") {
            let mut it = v.splitn(2, '\t');
            if let (Some(path), Some(n)) = (it.next(), it.next()) {
                if let Ok(n) = n.trim().parse::<i32>() {
                    if !path.is_empty() && n >= 1 {
                        proj_numbers.insert(path.to_string(), n);
                    }
                }
            }
        } else if let Some(v) = line.strip_prefix("p=") {
            let parts: Vec<&str> = v.splitn(3, '\t').collect();
            if parts.len() >= 2 {
                let project = parts[0].to_string();
                let color = parts[1].trim().parse::<i32>().unwrap_or(-1);
                let label = parts.get(2).map(|s| s.to_string()).unwrap_or_default();
                projects.insert(project, ProjConf { color, label });
            }
        }
    }
    // END_BLOCK_PARSE_LINES
    ParsedIni { projects, proj_numbers, collapsed, recent_expanded, recent_showall, section_order, window_order, font_face, font_size, font_weight, pos, search_db, search_cmd, search_port, chats_db, files_db, projects_root, search_files }
}

impl Config {
    pub fn load(cfg_path: PathBuf) -> Config {
        let text = std::fs::read_to_string(&cfg_path).unwrap_or_default();
        let p = parse_ini(&text);
        Config {
            apps: default_apps(),
            projects: p.projects,
            proj_numbers: p.proj_numbers,
            collapsed: p.collapsed,
            recent_expanded: p.recent_expanded,
            recent_showall: p.recent_showall,
            section_order: p.section_order,
            window_order: p.window_order,
            font_face: p.font_face,
            font_size: p.font_size,
            font_weight: p.font_weight,
            pos: p.pos,
            search_db: p.search_db,
            search_cmd: p.search_cmd,
            search_port: p.search_port,
            chats_db: p.chats_db,
            files_db: p.files_db,
            projects_root: p.projects_root,
            search_files: p.search_files,
            cfg_path,
        }
    }

    pub fn serialize(&self, pos: Option<(i32, i32)>) -> String {
        let mut out = String::from("# claudebar config\n");
        if let Some((x, y)) = pos {
            out += &format!("pos={},{}\n", x, y);
        }
        out += &format!("font={}\t{}\t{}\n", self.font_face, self.font_size, self.font_weight);
        out += &format!("searchdb={}\n", self.search_db);
        out += &format!("searchcmd={}\n", self.search_cmd);
        out += &format!("searchport={}\n", self.search_port);
        out += &format!("chatsdb={}\n", self.chats_db);
        out += &format!("filesdb={}\n", self.files_db);
        out += &format!("projectsroot={}\n", self.projects_root);
        out += &format!("searchfiles={}\n", if self.search_files { 1 } else { 0 });
        for block in &self.collapsed {
            out += &format!("c={}\n", block);
        }
        for block in &self.recent_expanded {
            out += &format!("re={}\n", block);
        }
        for block in &self.recent_showall {
            out += &format!("ra={}\n", block);
        }
        if !self.section_order.is_empty() {
            out += &format!("os={}\n", self.section_order.join("\t"));
        }
        for (block, names) in &self.window_order {
            if !names.is_empty() {
                out += &format!("o={}\t{}\n", block, names.join("\t"));
            }
        }
        for (project, c) in &self.projects {
            if c.color < 0 && c.label.is_empty() {
                continue;
            }
            out += &format!("p={}\t{}\t{}\n", project, c.color, c.label);
        }
        for (path, n) in &self.proj_numbers {
            out += &format!("pn={}\t{}\n", path, n);
        }
        out
    }

    pub fn save(&self, hwnd: HWND) {
        let mut pos = self.pos;
        let mut rc = RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut rc) }.is_ok() {
            pos = Some((rc.left, rc.top));
        }
        let _ = std::fs::write(&self.cfg_path, self.serialize(pos));
    }

    pub fn is_collapsed(&self, block: &str) -> bool {
        self.collapsed.contains(block)
    }

    pub fn toggle_collapsed(&mut self, block: &str) {
        if !self.collapsed.remove(block) {
            self.collapsed.insert(block.to_string());
        }
    }

    pub fn is_recent_open(&self, block: &str) -> bool {
        self.recent_expanded.contains(block)
    }

    pub fn toggle_recent(&mut self, block: &str) {
        if !self.recent_expanded.remove(block) {
            self.recent_expanded.insert(block.to_string());
        }
    }

    pub fn is_showall(&self, block: &str) -> bool {
        self.recent_showall.contains(block)
    }

    pub fn toggle_showall(&mut self, block: &str) {
        if !self.recent_showall.remove(block) {
            self.recent_showall.insert(block.to_string());
        }
    }

    // Задать шрифт панели (гарнитура + кегль + насыщенность). Значения ограничены разумным диапазоном.
    pub fn set_font(&mut self, face: &str, size: i32, weight: i32) {
        if !face.trim().is_empty() {
            self.font_face = face.to_string();
        }
        self.font_size = size.clamp(6, 72);
        self.font_weight = weight.clamp(100, 900);
    }

    // Индексы секций в ручном порядке: сперва известные по section_order, затем остальные по исходному.
    pub fn section_index_order(&self, apps: &[AppDef]) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::with_capacity(apps.len());
        for block in &self.section_order {
            if let Some(i) = apps.iter().position(|a| &a.block == block) {
                if !out.contains(&i) {
                    out.push(i);
                }
            }
        }
        for i in 0..apps.len() {
            if !out.contains(&i) {
                out.push(i);
            }
        }
        out
    }

    // Позиция имени окна в ручном порядке секции (для сортировки); None = нет порядка.
    pub fn window_rank(&self, block: &str, name: &str) -> Option<usize> {
        self.window_order.get(block).and_then(|names| names.iter().position(|n| n == name))
    }

    // Перестановка секции: дан текущий видимый порядок блоков, переставить from->to, сохранить.
    pub fn move_section(&mut self, current_blocks: &[String], from: usize, to: usize) {
        let mut order: Vec<String> = current_blocks.to_vec();
        reorder_vec(&mut order, from, to);
        self.section_order = order;
    }

    // Перестановка окна внутри секции.
    pub fn move_window(&mut self, block: &str, current_names: &[String], from: usize, to: usize) {
        let mut order: Vec<String> = current_names.to_vec();
        reorder_vec(&mut order, from, to);
        self.window_order.insert(block.to_string(), order);
    }

    pub fn color_idx(&self, project: &str) -> usize {
        match self.projects.get(project) {
            Some(c) if c.color >= 0 => (c.color as usize).min(PALETTE.len() - 1),
            _ => auto_color(project),
        }
    }

    pub fn label(&self, project: &str) -> String {
        self.projects.get(project).map(|c| c.label.clone()).unwrap_or_default()
    }

    pub fn set_color(&mut self, project: &str, idx: usize) {
        self.projects.entry(project.to_string()).or_default().color = idx as i32;
    }

    pub fn set_label(&mut self, project: &str, label: String) {
        self.projects.entry(project.to_string()).or_default().label = label;
    }

    // Стабильный № дубля для полного пути проекта: первый среди одноимённых basename = 1, далее +1.
    // Persisted (pn=), НИКОГДА не переназначается: повторный вызов для того же пути вернёт тот же №.
    pub fn assign_number(&mut self, path: &str) -> i32 {
        if let Some(&n) = self.proj_numbers.get(path) {
            return n;
        }
        let base = base_key(path);
        let next = self.proj_numbers.iter().filter(|(p, _)| base_key(p) == base).map(|(_, n)| *n).max().unwrap_or(0) + 1;
        self.proj_numbers.insert(path.to_string(), next);
        next
    }

    pub fn number_for(&self, path: &str) -> Option<i32> {
        self.proj_numbers.get(path).copied()
    }

    // Цвет по ключу-идентичности (полный путь), с откатом на fallback (имя) — back-compat со старым конфигом.
    pub fn color_idx_for(&self, key: &str, fallback: &str) -> usize {
        if self.projects.contains_key(key) {
            self.color_idx(key)
        } else if self.projects.contains_key(fallback) {
            self.color_idx(fallback)
        } else {
            auto_color(fallback)
        }
    }

    pub fn label_for(&self, key: &str, fallback: &str) -> String {
        if self.projects.contains_key(key) {
            self.label(key)
        } else {
            self.label(fallback)
        }
    }
}

// basename проекта в нижнем регистре — группировка одноимённых путей для нумерации.
fn base_key(path: &str) -> String {
    path.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or(path).to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(projects: Vec<(&str, i32, &str)>) -> Config {
        let mut map = HashMap::new();
        for (p, c, l) in projects {
            map.insert(p.to_string(), ProjConf { color: c, label: l.to_string() });
        }
        Config {
            apps: default_apps(),
            projects: map,
            proj_numbers: HashMap::new(),
            collapsed: HashSet::new(),
            recent_expanded: HashSet::new(),
            recent_showall: HashSet::new(),
            section_order: Vec::new(),
            window_order: HashMap::new(),
            font_face: DEFAULT_FONT.to_string(),
            font_size: DEFAULT_FONT_SIZE,
            font_weight: DEFAULT_FONT_WEIGHT,
            pos: None,
            search_db: String::new(),
            search_cmd: String::new(),
            search_port: DEFAULT_SEARCH_PORT,
            chats_db: String::new(),
            files_db: String::new(),
            projects_root: String::new(),
            search_files: false,
            cfg_path: PathBuf::new(),
        }
    }

    #[test]
    fn project_number_stable_and_keyed() {
        let mut c = cfg(vec![]);
        let (a, b, other) = ("D:\\a\\claudebar", "E:\\b\\claudebar", "D:\\x\\hh");
        assert_eq!(c.assign_number(a), 1); // первый claudebar
        assert_eq!(c.assign_number(b), 2); // второй одноимённый
        assert_eq!(c.assign_number(other), 1); // другой basename -> свой счёт
        assert_eq!(c.assign_number(a), 1); // стабильность: тот же путь -> тот же №
        assert_eq!(c.assign_number(b), 2);
        assert_eq!(c.number_for(b), Some(2));
        assert_eq!(c.number_for("нет"), None);
        // round-trip через ini
        let p = parse_ini(&c.serialize(None));
        assert_eq!(p.proj_numbers.get(a), Some(&1));
        assert_eq!(p.proj_numbers.get(b), Some(&2));
    }

    #[test]
    fn color_label_by_path_with_fallback() {
        let mut c = cfg(vec![("claudebar", 3, "old")]); // старый конфиг по имени
        // окно с путём, записи по пути нет -> fallback на имя
        assert_eq!(c.color_idx_for("D:\\a\\claudebar", "claudebar"), 3);
        assert_eq!(c.label_for("D:\\a\\claudebar", "claudebar"), "old");
        // задаём по пути -> приоритет у пути
        c.set_color("D:\\a\\claudebar", 5);
        assert_eq!(c.color_idx_for("D:\\a\\claudebar", "claudebar"), 5);
        assert_eq!(c.color_idx_for("claudebar", "claudebar"), 3); // окно без пути -> старое
    }

    #[test]
    fn auto_color_in_range_and_deterministic() {
        for n in ["A", "Test_2026.05.28", "ConstructMan", ""] {
            assert!(auto_color(n) < PALETTE.len());
            assert_eq!(auto_color(n), auto_color(n));
        }
    }

    #[test]
    fn color_idx_uses_set_or_auto_and_clamps() {
        let mut c = cfg(vec![]);
        assert_eq!(c.color_idx("Proj"), auto_color("Proj"));
        c.set_color("Proj", 3);
        assert_eq!(c.color_idx("Proj"), 3);
        c.set_color("Proj", 100);
        assert_eq!(c.color_idx("Proj"), PALETTE.len() - 1);
    }

    #[test]
    fn label_set_get_clear() {
        let mut c = cfg(vec![]);
        assert_eq!(c.label("Proj"), "");
        c.set_label("Proj", "opus".into());
        assert_eq!(c.label("Proj"), "opus");
        c.set_label("Proj", String::new());
        assert_eq!(c.label("Proj"), "");
    }

    #[test]
    fn collapse_toggles_and_persists() {
        let mut c = cfg(vec![]);
        assert!(!c.is_collapsed("Excel"));
        c.toggle_collapsed("Excel");
        assert!(c.is_collapsed("Excel"));
        // round-trip через сериализацию
        let text = c.serialize(Some((1, 2)));
        let p = parse_ini(&text);
        assert!(p.collapsed.contains("Excel"));
        // повторный toggle снимает
        c.toggle_collapsed("Excel");
        assert!(!c.is_collapsed("Excel"));
    }

    #[test]
    fn showall_toggles_and_persists() {
        let mut c = cfg(vec![]);
        assert!(!c.is_showall("Word"));
        c.toggle_showall("Word");
        assert!(c.is_showall("Word"));
        // round-trip через ra=
        let text = c.serialize(Some((0, 0)));
        let p = parse_ini(&text);
        assert!(p.recent_showall.contains("Word"));
        // повторный toggle снимает
        c.toggle_showall("Word");
        assert!(!c.is_showall("Word"));
    }

    #[test]
    fn section_order_known_first_then_rest() {
        let mut c = cfg(vec![]);
        let apps = default_apps(); // Code, Cursor, Word, Excel, MS Project, затем терминалы/Проводник
        let n = apps.len();
        // по умолчанию — исходный порядок (устойчиво к числу приложений)
        let ident: Vec<usize> = (0..n).collect();
        assert_eq!(c.section_index_order(&apps), ident);
        // Word (2) и Excel (3) вперёд, остальные сохраняют исходный порядок
        c.section_order = vec!["Word".into(), "Excel".into()];
        let mut expected = vec![2usize, 3];
        expected.extend((0..n).filter(|i| *i != 2 && *i != 3));
        assert_eq!(c.section_index_order(&apps), expected);
    }

    #[test]
    fn window_order_rank_and_move_roundtrip() {
        let mut c = cfg(vec![]);
        assert_eq!(c.window_rank("VS Code", "A"), None);
        // выставить порядок перестановкой: [A,B,C] -> двинуть C(2) в начало(0)
        c.move_window("VS Code", &["A".into(), "B".into(), "C".into()], 2, 0);
        assert_eq!(c.window_rank("VS Code", "C"), Some(0));
        assert_eq!(c.window_rank("VS Code", "A"), Some(1));
        // персист через o=
        let text = c.serialize(None);
        let p = parse_ini(&text);
        assert_eq!(p.window_order.get("VS Code").unwrap(), &vec!["C".to_string(), "A".into(), "B".into()]);
    }

    #[test]
    fn font_roundtrip_and_default() {
        let mut c = cfg(vec![]);
        // дефолт без font= в ini
        assert_eq!(c.font_face, DEFAULT_FONT);
        assert_eq!(c.font_size, DEFAULT_FONT_SIZE);
        // set + round-trip через font= (гарнитура с пробелом сохраняется)
        c.set_font("Cascadia Mono", 18, 500);
        let text = c.serialize(None);
        let p = parse_ini(&text);
        assert_eq!(p.font_face, "Cascadia Mono");
        assert_eq!(p.font_size, 18);
        // отсутствие font= -> дефолт
        let p2 = parse_ini("pos=1,2\n");
        assert_eq!(p2.font_face, DEFAULT_FONT);
        assert_eq!(p2.font_size, DEFAULT_FONT_SIZE);
        // кегль за пределами диапазона клампится
        c.set_font("X", 999, 600);
        assert_eq!(c.font_size, 72);
    }

    #[test]
    fn font_weight_persisted_and_honored() {
        // regression: вес шрифта не сохранялся -> панель всегда жирная (600), диалог не предзаполнял стиль
        let mut c = cfg(vec![]);
        assert_eq!(c.font_weight, DEFAULT_FONT_WEIGHT); // 600 по умолчанию (текущий вид сохраняется)
        c.set_font("Iosevka Fixed", 15, 400); // выбрали обычный начертание
        assert_eq!(c.font_weight, 400);
        let text = c.serialize(None);
        let p = parse_ini(&text);
        assert_eq!(p.font_weight, 400);
        // обратная совместимость: старый font= из 2 полей -> вес по умолчанию
        let p2 = parse_ini("font=Iosevka Fixed\t16\n");
        assert_eq!(p2.font_face, "Iosevka Fixed");
        assert_eq!(p2.font_size, 16);
        assert_eq!(p2.font_weight, DEFAULT_FONT_WEIGHT);
    }

    #[test]
    fn offscreen_pos_falls_back_to_default() {
        // regression: после смены конфигурации мониторов сохранённая позиция оказывается
        // вне видимой области -> окно создаётся за экраном (видно в панели задач, не на экране).
        let default = (1600, 40);
        let (w, h) = (300, 600);
        // один монитор 1920x1080 в начале координат
        let (vx, vy, vw, vh) = (0, 0, 1920, 1080);

        // позиция на отключённом втором мониторе справа -> сброс на дефолт
        assert_eq!(
            visible_start_pos(Some((3000, 100)), default, w, h, vx, vy, vw, vh),
            default,
            "позиция правее всех экранов должна сброситься на дефолт"
        );
        // позиция слева за пределами всех экранов -> дефолт
        assert_eq!(
            visible_start_pos(Some((-2000, 100)), default, w, h, vx, vy, vw, vh),
            default
        );
        // позиция ниже всех экранов -> дефолт
        assert_eq!(
            visible_start_pos(Some((100, 5000)), default, w, h, vx, vy, vw, vh),
            default
        );
        // валидная видимая позиция сохраняется как есть
        assert_eq!(
            visible_start_pos(Some((100, 100)), default, w, h, vx, vy, vw, vh),
            (100, 100)
        );
        // частично за краем, но видимой полосы хватает (>= MIN_VISIBLE) -> сохраняется
        assert_eq!(
            visible_start_pos(Some((1800, 100)), default, w, h, vx, vy, vw, vh),
            (1800, 100)
        );
        // None -> дефолт
        assert_eq!(visible_start_pos(None, default, w, h, vx, vy, vw, vh), default);
    }

    #[test]
    fn serialize_parse_roundtrip() {
        let c = cfg(vec![("Proj A", 3, "opus"), ("Empty", -1, "")]);
        let text = c.serialize(Some((10, 20)));
        let p = parse_ini(&text);
        assert_eq!(p.pos, Some((10, 20)));
        let a = p.projects.get("Proj A").unwrap();
        assert_eq!(a.color, 3);
        assert_eq!(a.label, "opus");
        assert!(p.projects.get("Empty").is_none());
    }

    #[test]
    fn search_config_roundtrip_and_defaults() {
        // дефолты без ключей в ini
        let p = parse_ini("");
        assert_eq!(p.search_port, DEFAULT_SEARCH_PORT);
        assert!(p.search_db.ends_with("clfind.db"));
        assert!(p.search_cmd.contains("clfind"));
        // round-trip serialize -> parse
        let mut c = cfg(vec![]);
        c.search_db = "D:\\idx\\clfind.db".into();
        c.search_cmd = "pythonw.exe -m clfind.cli serve".into();
        c.search_port = 9100;
        let p2 = parse_ini(&c.serialize(None));
        assert_eq!(p2.search_db, "D:\\idx\\clfind.db");
        assert_eq!(p2.search_cmd, "pythonw.exe -m clfind.cli serve");
        assert_eq!(p2.search_port, 9100);
        // Phase-13: свои базы + корень индексации — дефолты и round-trip
        assert!(p.chats_db.ends_with("claudebar_chats.db"));
        assert!(p.projects_root.ends_with("projects"));
        c.chats_db = "D:\\idx\\chats.db".into();
        c.projects_root = "D:\\proj".into();
        let p3 = parse_ini(&c.serialize(None));
        assert_eq!(p3.chats_db, "D:\\idx\\chats.db");
        assert_eq!(p3.projects_root, "D:\\proj");
        // search_files: дефолт false, round-trip
        assert!(!p.search_files);
        c.search_files = true;
        let p4 = parse_ini(&c.serialize(None));
        assert!(p4.search_files);
    }

    #[test]
    fn default_apps_cover_expected_processes() {
        let apps = default_apps();
        let procs: Vec<&str> = apps.iter().map(|a| a.proc.as_str()).collect();
        for p in ["code.exe", "cursor.exe", "winword.exe", "excel.exe", "winproj.exe"] {
            assert!(procs.contains(&p), "missing {p}");
        }
    }

    #[test]
    fn default_apps_include_terminals_and_explorer() {
        let apps = default_apps();
        let find = |block: &str| apps.iter().find(|a| a.block == block);
        let wt = find("Windows Terminal").expect("Windows Terminal");
        assert_eq!(wt.proc, "windowsterminal.exe");
        assert!(wt.class.is_none());
        let cmd = find("Командная строка").expect("cmd");
        assert_eq!(cmd.class.as_deref(), Some("ConsoleWindowClass"));
        let ps = find("PowerShell").expect("PowerShell");
        assert!(ps.proc_alts.iter().any(|p| p == "pwsh.exe"));
        assert_eq!(ps.class.as_deref(), Some("ConsoleWindowClass"));
        let expl = find("Проводник").expect("explorer");
        assert_eq!(expl.proc, "explorer.exe");
        assert_eq!(expl.class.as_deref(), Some("CabinetWClass"));
        assert!(matches!(expl.mode, NameMode::Whole));
        find("Git Bash").expect("Git Bash");
    }
}
