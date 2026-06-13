// FILE: src/config.rs
// VERSION: 1.6.0
// START_MODULE_CONTRACT
//   PURPOSE: Конфигурация: приложения (по процессу), настройки проектов (цвет, метка), свёрнутость секций, «показать все» недавних, позиция, шрифт панели.
//   SCOPE: палитра, авто-цвет, AppDef/NameMode, дефолтный набор приложений, свёрнутость секций, раскрытие/showall недавних, шрифт (font_face/font_size), парсинг/сериализация ini.
//   DEPENDS: none (Win32 только для чтения позиции окна при сохранении)
//   LINKS: M-CONFIG
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   Config        - корень конфигурации: apps, projects, collapsed, recent_expanded, recent_showall, section_order, window_order, font_face, font_size, pos, cfg_path
//   AppDef        - приложение: процесс, имя блока, правило имени, расширения
//   NameMode      - извлечение имени: Project{suffix} | Document
//   set_font / font_face / font_size   - шрифт панели (ключ font=), дефолт Iosevka Fixed/16
//   ProjConf      - настройки проекта: индекс цвета (-1 = авто), метка
//   PALETTE       - палитра из 8 цветов
//   auto_color    - детерминированный цвет по имени
//   default_apps  - встроенный набор приложений
//   section_index_order / window_rank - применение ручного порядка
//   move_section / move_window         - перестановка при drag-reorder
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.6.0 - Phase-9 Step 1: шрифт панели (font_face/font_size, ключ font=, деф. Iosevka Fixed/16); set_font; round-trip.
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
}

#[derive(Clone)]
pub struct AppDef {
    pub proc: String, // имя exe в нижнем регистре, напр. "code.exe"
    pub block: String, // отображаемое имя секции
    pub mode: NameMode, // правило извлечения имени
    pub exts: Vec<String>, // расширения недавних документов (Office)
    pub editor_storage: Option<String>, // для редакторов: подпапка в %APPDATA% (Code/Cursor) — недавние проекты
}

pub struct Config {
    pub apps: Vec<AppDef>,
    pub projects: HashMap<String, ProjConf>,
    pub collapsed: HashSet<String>, // имена свёрнутых секций (block)
    pub recent_expanded: HashSet<String>, // секции с раскрытым под-блоком «недавние»
    pub recent_showall: HashSet<String>, // секции с раскрытым полным списком недавних (сверх 6)
    pub section_order: Vec<String>, // ручной порядок секций (block); пустой = по умолчанию
    pub window_order: HashMap<String, Vec<String>>, // block -> ручной порядок имён окон
    pub font_face: String, // гарнитура шрифта панели (ключ font=)
    pub font_size: i32, // базовый кегль (px); мелкий шрифт = font_size-3
    pub pos: Option<(i32, i32)>,
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
            block: block.to_string(),
            mode,
            exts: exts.iter().map(|s| s.to_string()).collect(),
            editor_storage: editor.map(|s| s.to_string()),
        }
    }
    vec![
        app("code.exe", "VS Code", NameMode::Project { suffix: " - Visual Studio Code".into() }, &[], Some("Code")),
        app("cursor.exe", "Cursor", NameMode::Project { suffix: " - Cursor".into() }, &[], Some("Cursor")),
        app("winword.exe", "Word", NameMode::Document, &["docx", "doc", "rtf"], None),
        app("excel.exe", "Excel", NameMode::Document, &["xlsx", "xls", "csv"], None),
        app("winproj.exe", "MS Project", NameMode::DocumentLast, &["mpp"], None),
    ]
}

struct ParsedIni {
    projects: HashMap<String, ProjConf>,
    collapsed: HashSet<String>,
    recent_expanded: HashSet<String>,
    recent_showall: HashSet<String>,
    section_order: Vec<String>,
    window_order: HashMap<String, Vec<String>>,
    font_face: String,
    font_size: i32,
    pos: Option<(i32, i32)>,
}

fn parse_ini(text: &str) -> ParsedIni {
    let mut projects: HashMap<String, ProjConf> = HashMap::new();
    let mut collapsed: HashSet<String> = HashSet::new();
    let mut recent_expanded: HashSet<String> = HashSet::new();
    let mut recent_showall: HashSet<String> = HashSet::new();
    let mut section_order: Vec<String> = Vec::new();
    let mut window_order: HashMap<String, Vec<String>> = HashMap::new();
    let mut font_face: String = DEFAULT_FONT.to_string();
    let mut font_size: i32 = DEFAULT_FONT_SIZE;
    let mut pos: Option<(i32, i32)> = None;
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
            // font=<гарнитура>\t<кегль>; гарнитура может содержать пробелы, но не табы
            let mut it = v.splitn(2, '\t');
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
    ParsedIni { projects, collapsed, recent_expanded, recent_showall, section_order, window_order, font_face, font_size, pos }
}

impl Config {
    pub fn load(cfg_path: PathBuf) -> Config {
        let text = std::fs::read_to_string(&cfg_path).unwrap_or_default();
        let p = parse_ini(&text);
        Config {
            apps: default_apps(),
            projects: p.projects,
            collapsed: p.collapsed,
            recent_expanded: p.recent_expanded,
            recent_showall: p.recent_showall,
            section_order: p.section_order,
            window_order: p.window_order,
            font_face: p.font_face,
            font_size: p.font_size,
            pos: p.pos,
            cfg_path,
        }
    }

    pub fn serialize(&self, pos: Option<(i32, i32)>) -> String {
        let mut out = String::from("# claudebar config\n");
        if let Some((x, y)) = pos {
            out += &format!("pos={},{}\n", x, y);
        }
        out += &format!("font={}\t{}\n", self.font_face, self.font_size);
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

    // Задать шрифт панели (гарнитура + кегль). Кегль ограничен разумным диапазоном.
    pub fn set_font(&mut self, face: &str, size: i32) {
        if !face.trim().is_empty() {
            self.font_face = face.to_string();
        }
        self.font_size = size.clamp(6, 72);
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
            collapsed: HashSet::new(),
            recent_expanded: HashSet::new(),
            recent_showall: HashSet::new(),
            section_order: Vec::new(),
            window_order: HashMap::new(),
            font_face: DEFAULT_FONT.to_string(),
            font_size: DEFAULT_FONT_SIZE,
            pos: None,
            cfg_path: PathBuf::new(),
        }
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
        let apps = default_apps(); // [Code, Cursor, Word, Excel, MS Project] = 0..5
        // по умолчанию — исходный порядок
        assert_eq!(c.section_index_order(&apps), vec![0, 1, 2, 3, 4]);
        // Word (2) и Excel (3) вперёд
        c.section_order = vec!["Word".into(), "Excel".into()];
        assert_eq!(c.section_index_order(&apps), vec![2, 3, 0, 1, 4]);
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
        c.set_font("Cascadia Mono", 18);
        let text = c.serialize(None);
        let p = parse_ini(&text);
        assert_eq!(p.font_face, "Cascadia Mono");
        assert_eq!(p.font_size, 18);
        // отсутствие font= -> дефолт
        let p2 = parse_ini("pos=1,2\n");
        assert_eq!(p2.font_face, DEFAULT_FONT);
        assert_eq!(p2.font_size, DEFAULT_FONT_SIZE);
        // кегль за пределами диапазона клампится
        c.set_font("X", 999);
        assert_eq!(c.font_size, 72);
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
    fn default_apps_cover_expected_processes() {
        let apps = default_apps();
        let procs: Vec<&str> = apps.iter().map(|a| a.proc.as_str()).collect();
        for p in ["code.exe", "cursor.exe", "winword.exe", "excel.exe", "winproj.exe"] {
            assert!(procs.contains(&p), "missing {p}");
        }
    }
}
