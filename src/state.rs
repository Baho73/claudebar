// FILE: src/state.rs
// VERSION: 1.1.0
// START_MODULE_CONTRACT
//   PURPOSE: Состояние панели: структура App (окна/недавние/поиск/сигналы/голос) и thread_local APP.
//   SCOPE: тип App (данные UI-состояния) + APP (RefCell<Option<App>> на UI-потоке). Логики нет — держатель состояния.
//   DEPENDS: M-CONFIG, M-WINENUM, M-RECENT, M-RENDER, M-SEARCH, M-SIGNAL, M-VOICE
//   LINKS: M-STATE
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
//   NOTE: Вынесено из main.rs (рефактор god-object, FPF D-5) — развязка, чтобы UI-куски (tooltip/searchbox/menu/prompt) могли импортировать App и отделиться в свои файлы.
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   App - состояние панели: 30+ полей (окна, недавние, строки, конфиг, шрифты, hover/drag, сигналы bell/busy, сессии, поиск+история, голос)
//   APP - thread_local RefCell<Option<App>> — единственный экземпляр состояния на UI-потоке
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.1.0 - поля whisper_ok (Phase-27) и mic_error (отказ захвата) — источники нижнего баннера.
//   v1.0.0 - рефактор: App/APP вынесены из main.rs в отдельный модуль состояния (M-STATE), без изменения поведения.
// END_CHANGE_SUMMARY

use std::cell::RefCell;
use std::collections::HashSet;

use windows::Win32::Foundation::{HINSTANCE, HWND};
use windows::Win32::Graphics::Gdi::HFONT;

use crate::config::Config;
use crate::{recent, render, search, signal, voice, win_enum};

// ---------- состояние ----------
pub(crate) struct App {
    pub(crate) hinst: HINSTANCE,
    pub(crate) items: Vec<win_enum::WinItem>,
    pub(crate) recent: Vec<recent::RecentDoc>,
    pub(crate) rows: Vec<render::Row>,
    pub(crate) config: Config,
    pub(crate) font_main: HFONT,
    pub(crate) font_small: HFONT,
    pub(crate) hover: i32,
    pub(crate) menu_target: usize, // индекс строки, по которой открыли меню
    pub(crate) menu_link: Option<(String, bool)>, // цель «ссылка/проводник»: (путь, is_file) — Phase-14
    pub(crate) last_h: i32,
    pub(crate) bell: HashSet<String>, // имена проектов со «звоночком» (lower) — подсветка строк (fallback)
    pub(crate) bell_paths: HashSet<String>, // полные cwd со «звоночком» (lower) — точная подсветка по пути (Phase-15)
    pub(crate) busy: HashSet<String>, // имена проектов с активным .busy (lower) — бегущие точки (fallback) — Phase-17
    pub(crate) busy_paths: HashSet<String>, // полные cwd с активным .busy (lower) — точные точки по пути — Phase-17
    pub(crate) sessions: Vec<signal::Sess>, // живые сессии-агенты (Phase-26 presence): агент + состояние на квадрат
    pub(crate) anim_frame: u32, // кадр анимации бегущих точек — Phase-17
    pub(crate) search_hits: Vec<search::FolderHit>, // папки-совпадения поиска (Phase-12)
    pub(crate) search_edit: HWND, // EDIT-поле поиска в шапке (null = скрыто)
    pub(crate) tooltip: HWND, // tracking-подсказка (путь/сниппет/правила) — Phase-13 Ф-B
    pub(crate) tip_row: i32, // под подсказкой: -1 нет, -2 строка поиска, >=0 индекс строки
    pub(crate) search_history: Vec<String>, // недавние запросы (свежие первыми) — Phase-13
    pub(crate) hist_list: HWND, // выпадающий список истории (child LISTBOX, скрыт)
    pub(crate) reorder: bool, // режим перетаскивания: ручки видны, ✕ скрыт
    pub(crate) drag: Option<i32>, // индекс перетаскиваемой строки во время drag
    pub(crate) voice: voice::Voice, // голосовой ввод: стейт-машина + активный Recorder — Phase-19
    pub(crate) voice_target: HWND, // окно-получатель вставки (foreground на старте записи) — Phase-19
    pub(crate) whisper_ok: bool, // сервер распознавания поднят (фоновый /health) — баннер-предупреждение, Phase-27
    pub(crate) mic_error: String, // текст отказа захвата микрофона (пусто = порядок) — баннер, fix 2026-08-06
}

thread_local! {
    pub(crate) static APP: RefCell<Option<App>> = RefCell::new(None);
}
