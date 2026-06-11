// FILE: src/config.rs
// VERSION: 1.0.0
// START_MODULE_CONTRACT
//   PURPOSE: Загрузка/сохранение claudebar.ini и доступ к настройкам проектов (цвет, метка).
//   SCOPE: парсинг/сериализация ini, палитра цветов, авто-цвет по имени, позиция панели, шаблоны заголовков.
//   DEPENDS: none (Win32 только для чтения позиции окна при сохранении)
//   LINKS: M-CONFIG
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   Config        - корень конфигурации: patterns, projects, pos, cfg_path
//   ProjConf      - настройки проекта: индекс цвета (-1 = авто), метка
//   PALETTE       - палитра из 8 цветов проектов (имя, r, g, b)
//   auto_color    - детерминированный цвет по имени проекта
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.0.0 - Выделено из монолита main.rs (Phase-1, Step 1). Паритет v0.1: формат ini и поведение сохранены.
// END_CHANGE_SUMMARY

use std::collections::HashMap;
use std::path::PathBuf;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

// палитра цветов проектов: (имя, r, g, b)
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

pub struct Config {
    pub patterns: Vec<String>,
    pub projects: HashMap<String, ProjConf>,
    pub pos: Option<(i32, i32)>,
    pub cfg_path: PathBuf,
}

// START_CONTRACT: auto_color
//   PURPOSE: Детерминированный индекс цвета по имени проекта (стабилен между запусками).
//   INPUTS: { name: &str - имя проекта }
//   OUTPUTS: { usize - индекс в PALETTE }
//   SIDE_EFFECTS: none
// END_CONTRACT: auto_color
pub fn auto_color(name: &str) -> usize {
    let h = name
        .bytes()
        .fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    (h % PALETTE.len() as u32) as usize
}

fn default_patterns() -> Vec<String> {
    vec![
        " - Visual Studio Code".to_string(),
        " - Cursor".to_string(),
    ]
}

// START_CONTRACT: parse_ini
//   PURPOSE: Чистый парсинг текста claudebar.ini в (patterns, projects, pos).
//   INPUTS: { text: &str - содержимое ini }
//   OUTPUTS: { (Vec<String>, HashMap<String,ProjConf>, Option<(i32,i32)>) }
//   SIDE_EFFECTS: none
// END_CONTRACT: parse_ini
fn parse_ini(text: &str) -> (Vec<String>, HashMap<String, ProjConf>, Option<(i32, i32)>) {
    let mut patterns: Vec<String> = Vec::new();
    let mut projects: HashMap<String, ProjConf> = HashMap::new();
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
        } else if let Some(v) = line.strip_prefix("pattern=") {
            if !v.is_empty() {
                patterns.push(v.to_string());
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
    (patterns, projects, pos)
}

impl Config {
    // START_CONTRACT: load
    //   PURPOSE: Прочитать claudebar.ini (или дефолты при отсутствии/ошибке).
    //   INPUTS: { cfg_path: PathBuf - путь к ini }
    //   OUTPUTS: { Config }
    //   SIDE_EFFECTS: чтение файла
    // END_CONTRACT: load
    pub fn load(cfg_path: PathBuf) -> Config {
        let text = std::fs::read_to_string(&cfg_path).unwrap_or_default();
        let (mut patterns, projects, pos) = parse_ini(&text);
        if patterns.is_empty() {
            patterns = default_patterns();
        }
        Config { patterns, projects, pos, cfg_path }
    }

    // START_CONTRACT: serialize
    //   PURPOSE: Чистая сериализация конфигурации в текст ini с заданной позицией.
    //   INPUTS: { pos: Option<(i32,i32)> - позиция панели }
    //   OUTPUTS: { String - содержимое ini }
    //   SIDE_EFFECTS: none
    // END_CONTRACT: serialize
    pub fn serialize(&self, pos: Option<(i32, i32)>) -> String {
        let mut out = String::from("# claudebar config\n");
        if let Some((x, y)) = pos {
            out += &format!("pos={},{}\n", x, y);
        }
        for p in &self.patterns {
            out += &format!("pattern={}\n", p);
        }
        for (project, c) in &self.projects {
            if c.color < 0 && c.label.is_empty() {
                continue;
            }
            out += &format!("p={}\t{}\t{}\n", project, c.color, c.label);
        }
        out
    }

    // START_CONTRACT: save
    //   PURPOSE: Сохранить конфигурацию в claudebar.ini, взяв позицию из окна панели.
    //   INPUTS: { hwnd: HWND - окно панели для чтения позиции }
    //   OUTPUTS: { () }
    //   SIDE_EFFECTS: запись файла; чтение позиции окна (GetWindowRect)
    // END_CONTRACT: save
    pub fn save(&self, hwnd: HWND) {
        let mut pos = self.pos;
        let mut rc = RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut rc) }.is_ok() {
            pos = Some((rc.left, rc.top));
        }
        let _ = std::fs::write(&self.cfg_path, self.serialize(pos));
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

    fn cfg(patterns: Vec<&str>, projects: Vec<(&str, i32, &str)>) -> Config {
        let mut map = HashMap::new();
        for (p, c, l) in projects {
            map.insert(p.to_string(), ProjConf { color: c, label: l.to_string() });
        }
        Config {
            patterns: patterns.into_iter().map(|s| s.to_string()).collect(),
            projects: map,
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
        let mut c = cfg(vec![], vec![]);
        // не задан -> авто
        assert_eq!(c.color_idx("Proj"), auto_color("Proj"));
        // задан -> он
        c.set_color("Proj", 3);
        assert_eq!(c.color_idx("Proj"), 3);
        // за пределами -> клампится
        c.set_color("Proj", 100);
        assert_eq!(c.color_idx("Proj"), PALETTE.len() - 1);
    }

    #[test]
    fn label_set_get_clear() {
        let mut c = cfg(vec![], vec![]);
        assert_eq!(c.label("Proj"), "");
        c.set_label("Proj", "opus".into());
        assert_eq!(c.label("Proj"), "opus");
        c.set_label("Proj", String::new());
        assert_eq!(c.label("Proj"), "");
    }

    #[test]
    fn serialize_parse_roundtrip() {
        let c = cfg(
            vec![" - Visual Studio Code", " - Cursor"],
            vec![("Proj A", 3, "opus"), ("Empty", -1, "")],
        );
        let text = c.serialize(Some((10, 20)));
        let (pat, proj, pos) = parse_ini(&text);
        assert_eq!(pos, Some((10, 20)));
        assert_eq!(pat, vec![" - Visual Studio Code", " - Cursor"]);
        let a = proj.get("Proj A").unwrap();
        assert_eq!(a.color, 3);
        assert_eq!(a.label, "opus");
        // проект без цвета и метки в файл не пишется
        assert!(proj.get("Empty").is_none());
    }

    #[test]
    fn parse_empty_has_no_patterns() {
        let (pat, proj, pos) = parse_ini("# just a comment\n");
        assert!(pat.is_empty());
        assert!(proj.is_empty());
        assert_eq!(pos, None);
    }
}
