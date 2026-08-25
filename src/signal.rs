// FILE: src/signal.rs
// VERSION: 1.5.3
// START_MODULE_CONTRACT
//   PURPOSE: «Звоночек» завершения ИИ: читать файлы-сигналы из %APPDATA%\claudebar\signals\, отдавать проекты для подсветки, гасить сигнал при фокусе окна проекта.
//   SCOPE: путь папки сигналов, парсинг .signal (cwd проекта), ключ проекта (basename) + полный cwd, наборы «звенящих» (basename и cwd), сброс по фокусу с матчем по полному пути.
//   DEPENDS: M-WINENUM (сопоставление сигнала с открытым окном по полному пути WinItem.path, иначе по basename)
//   EFFECTS: своё: создаёт %APPDATA%\claudebar\signals\ и УДАЛЯЕТ оттуда *.signal при фокусе окна проекта (reconcile); чужое: сами файлы *.busy/*.alive/*.signal пишут хуки агентов (hooks\claudebar-*.ps1), владение общее
//   REVERT: удалить каталог signals\ целиком; ОТДЕЛЬНО снять блоки хуков из ~/.claude/settings.json и ~/.kimi-code/config.toml — иначе зовут несуществующие скрипты. Долг: деинсталлятора хуков нет, а сами хуки вне src и контракта не имеют
//   LINKS: M-SIGNAL
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   Signal            - активный сигнал: путь файла, ключ проекта (basename, lower), полный cwd (lower)
//   signal_dir        - путь к %APPDATA%\claudebar\signals (создаётся при отсутствии)
//   parse_signal      - извлечь cwd проекта из содержимого .signal
//   project_key       - basename(cwd) в нижнем регистре — fallback-ключ сопоставления
//   should_clear      - чистое: сигнал гасится, если окно в фокусе — этот проект (cwd == путь окна ИЛИ вложен, иначе basename)
//   row_signaled      - чистое: «звенит/занят» ли строка для набора сигналов (cwd == путь ИЛИ вложен в путь; иначе basename)
//   path_within       - чистое (приват): путь child == ancestor или внутри него по границе сегмента
//   list_signals      - прочитать активные сигналы из папки
//   bell_keys         - множество «звенящих» basename-ключей для paint (fallback)
//   bell_cwds         - множество полных cwd активных сигналов — точная подсветка по пути (Phase-15)
//   is_stale          - чистое: устарел ли .busy по mtime (фильтр зависших) — Phase-17
//   list_ext          - чтение сигналов по расширению (signal|busy) с опц. staleness-фильтром
//   busy_keys/busy_cwds - наборы проектов с активным .busy (индикатор работы) — Phase-17
//   parse_agent       - агент из строки agent= файла-сигнала (Phase-26)
//   Sess/SessState    - живая сессия-агент (cwd, key, agent, state Idle|Working|Done) — Phase-26
//   sessions          - склейка .alive/.busy/.signal по session-id -> Vec<Sess> (состояние busy>signal>idle) — Phase-26
//   sess_matches_row  - чистое: относится ли сессия к строке окна (path_within/basename) — Phase-26
//   sort_sessions     - детерминированный порядок квадратов: Claude слева, Kimi справа, внутри — по sid
//   reconcile         - удалить .signal, чьё окно проекта сейчас foreground (по полному пути)
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.5.3 - fix(grace-fix): квадраты тасовались местами каждый опрос (порядок HashMap в sessions() недетерминирован). Sess += sid; sort_sessions фиксирует порядок: Claude слева, Kimi справа, внутри агента — по sid. Тест sort_sessions_claude_left_kimi_right.
//   v1.5.2 - cleanup(FPF D-27): снос busy_list/done_list/count_for_row (мертвы после Phase-26 — render на sessions(); слэш-покрытие осталось в parse_agent_and_sess_matches_row).
//   v1.5.1 - fix(grace-fix, FPF D-16): .alive читается с TTL ALIVE_STALE_SECS (12ч) — краш/ребут без graceful SessionEnd больше не оставляет вечный фантомный квадрат; busy-хук keep-alive трогает .alive, держа активные сессии свежими.
//   v1.5.0 - Phase-26 presence: Signal += agent (parse_agent из строки agent=); тип Sess/SessState + sessions() (склейка .alive присутствие + .busy работает + .signal готово по session-id, состояние busy>signal>idle); sess_matches_row (матч сессии к строке). Для квадратов «на каждого запущенного агента, свой цвет».
//   v1.4.1 - fix(grace-fix): path_within нормализует слэши (/ -> \). Kimi через Git Bash пишет cwd со слэшами (D:/Python/claudebar), Claude — с бэкслэшами; матч по пути не срабатывал -> квадрат/подсветка сессии Kimi пропадали. Тест count_for_row_normalizes_slashes.
//   v1.4.0 - Phase-25: посессионные списки busy_list()/done_list() (по одному на .busy/.signal-файл, С дублями) + чистая count_for_row(row_path,row_name,sessions) — сколько сессий на строке (path_within/basename, дубли суммируются, D-06). Для квадратов-статусов терминалов (Claude+Kimi). Тесты count_for_row_*.
//   v1.3.0 - fix(grace-fix): row_signaled + path_within — матч bell/busy строки, когда cwd сигнала ВЛОЖЕН в путь окна (Claude-сессия в подпапке открытого проекта, напр. окно D:\Python\Mosco, сессия D:\Python\Mosco\doc). Регрессия проявилась при включённом FULLPATHS: путь у строки появлялся -> матч становился только по точному пути, fallback на basename пропадал -> подсветка/точки гасли. should_clear тоже на path_within (фокус родителя гасит сигнал подпапки). D-06 (одноимённые в разных путях) сохранён.
//   v1.2.1 - Phase-17 hardening: staleness 600->90с + keep-alive (PostToolUse-хук обновляет .busy на каждом инструменте), чтобы точки держались всю длинную работу и гасли ~через 90с после смерти/Stop.
//   v1.2.0 - Phase-17 step-1: чтение .busy (индикатор работы) — list_ext(ext, ttl) + busy_keys/busy_cwds + is_stale (фильтр зависших по mtime >600с). list_signals переведён на list_ext.
//   v1.1.0 - Phase-15 step-3: матч звоночка по полному cwd == WinItem.path (fallback basename); чинит коллизию одноимённых (D-06). Signal += cwd; should_clear(sig_cwd,sig_key,fg_path,fg_key); bell_cwds.
//   v1.0.0 - Phase-4 Step 1: модуль сигналов «звоночка» (файловый IPC из Claude Code).
// END_CHANGE_SUMMARY

use std::collections::HashSet;
use std::path::PathBuf;

use windows::Win32::Foundation::HWND;

use crate::win_enum::WinItem;

pub struct Signal {
    pub path: PathBuf,
    pub key: String,   // имя проекта (lower) = basename(cwd) — fallback-матч
    pub cwd: String,   // полный cwd проекта (lower) — точный матч по пути (Phase-15)
    pub agent: String, // "claude" | "kimi" | "" — из строки agent= (Phase-26)
}

// Состояние сессии-агента для квадрата-статуса (Phase-26).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessState {
    Idle,    // запущен, но простаивает -> пустой контур
    Working, // .busy свежий -> контур + бегущая змейка
    Done,    // .signal (закончил) -> золотая заливка
}

// Живая сессия-агент: где, какой агент, в каком состоянии (Phase-26).
#[derive(Clone, Debug)]
pub struct Sess {
    pub sid: String,       // session-id (стабильный ключ сессии) — детерминированный порядок квадратов
    pub cwd: String,       // полный cwd (lower)
    pub key: String,       // basename (lower)
    pub agent: String,     // "claude" | "kimi" | ""
    pub state: SessState,
}

// START_CONTRACT: signal_dir
//   PURPOSE: Путь к папке сигналов (%APPDATA%\claudebar\signals), создать при отсутствии.
//   INPUTS: {}
//   OUTPUTS: { Option<PathBuf> - None если %APPDATA% не задан }
//   SIDE_EFFECTS: создаёт каталог (create_dir_all)
// END_CONTRACT: signal_dir
pub fn signal_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    let dir = PathBuf::from(base).join("claudebar").join("signals");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

// START_CONTRACT: parse_signal
//   PURPOSE: Извлечь cwd проекта из содержимого .signal (строка cwd=... или первая непустая).
//   INPUTS: { content: &str - содержимое файла-сигнала }
//   OUTPUTS: { Option<String> - путь cwd или None }
//   SIDE_EFFECTS: none
// END_CONTRACT: parse_signal
pub fn parse_signal(content: &str) -> Option<String> {
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("agent=") {
            continue; // agent= — не путь
        }
        let v = t.strip_prefix("cwd=").unwrap_or(t).trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    None
}

// Агент из строки "agent=..." файла-сигнала (Phase-26); нет строки -> "" (неизвестный).
pub fn parse_agent(content: &str) -> String {
    for line in content.lines() {
        if let Some(v) = line.trim().strip_prefix("agent=") {
            return v.trim().to_lowercase();
        }
    }
    String::new()
}

// START_CONTRACT: project_key
//   PURPOSE: Ключ проекта для сопоставления со строкой окна: basename(cwd) в нижнем регистре.
//   INPUTS: { cwd: &str - путь проекта }
//   OUTPUTS: { String - имя последнего сегмента пути, lower }
//   SIDE_EFFECTS: none
// END_CONTRACT: project_key
pub fn project_key(cwd: &str) -> String {
    cwd.trim_end_matches(['\\', '/', ' '])
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(cwd)
        .to_lowercase()
}

// START_CONTRACT: should_clear
//   PURPOSE: Решить, гасить ли сигнал: окно в фокусе — этот проект. По полному пути (точно), иначе по basename.
//   INPUTS: { sig_cwd: &str - полный cwd сигнала (lower); sig_key: &str - basename (lower); fg_path: Option<&str> - путь окна в фокусе; fg_key: &str - basename имени окна (lower) }
//   OUTPUTS: { bool }
//   SIDE_EFFECTS: none
// END_CONTRACT: should_clear
pub fn should_clear(sig_cwd: &str, sig_key: &str, fg_path: Option<&str>, fg_key: &str) -> bool {
    match fg_path {
        Some(p) => path_within(sig_cwd, p), // cwd сигнала == путь окна ИЛИ вложен в него (сессия в подпапке)
        None => fg_key == sig_key,          // окно без пути -> fallback на basename
    }
}

// START_CONTRACT: row_signaled
//   PURPOSE: Чистое — «звенит/занят» ли строка для набора сигналов: путь строки == cwd сигнала ИЛИ cwd сигнала
//            вложен в путь строки (Claude-сессия в подпапке открытого проекта: окно ...\Mosco, сессия ...\Mosco\doc).
//            Без пути у строки — fallback на basename. Одноимённые в РАЗНЫХ путях не путаются (D-06).
//   INPUTS: { row_path: Option<&str>; row_name: &str; sig_cwds: &HashSet<String> (lower); sig_keys: &HashSet<String> (lower) }
//   OUTPUTS: { bool }
//   SIDE_EFFECTS: none
//   LINKS: M-RENDER (подсветка bell/busy строк), M-SIGNAL
// END_CONTRACT: row_signaled
pub fn row_signaled(row_path: Option<&str>, row_name: &str, sig_cwds: &HashSet<String>, sig_keys: &HashSet<String>) -> bool {
    match row_path {
        Some(p) => sig_cwds.iter().any(|c| path_within(c, p)),
        None => sig_keys.contains(&row_name.to_lowercase()),
    }
}

// Чистое: путь `child` это `ancestor` ИЛИ лежит ВНУТРИ него по границе сегмента (регистронезависимо).
// Слэши нормализуются (/ -> \): Kimi через Git Bash пишет cwd со слэшами, Claude — с бэкслэшами.
// d:\x\mosco\doc within d:\x\mosco -> true; d:\x\mosco within d:\x\mos -> false (не граница сегмента);
// d:\y\mosco within d:\x\mosco -> false (D-06: одноимённые в разных путях не путаются).
fn path_within(child: &str, ancestor: &str) -> bool {
    let norm = |s: &str| s.to_lowercase().replace('/', "\\");
    let c = norm(child);
    let a = norm(ancestor);
    c == a || c.starts_with(&(a + "\\"))
}

// .busy старше этого возраста (с) считается зависшим и игнорируется. Keep-alive (PostToolUse-хук
// обновляет mtime на каждом инструменте) держит файл свежим, пока Claude реально работает — Phase-17.
pub const BUSY_STALE_SECS: u64 = 90;

// .alive старше этого возраста считается брошенным (краш/ребут без graceful SessionEnd) и игнорируется.
// keep-alive (busy-хук трогает .alive на каждом инструменте) держит активные сессии свежими — D-16.
pub const ALIVE_STALE_SECS: u64 = 12 * 3600;

// START_CONTRACT: is_stale
//   PURPOSE: Чистое: устарел ли файл-сигнал (mtime старше ttl от now) — фильтр зависших .busy (Phase-17).
//   INPUTS: { mtime_secs: u64; now_secs: u64; ttl: u64 }
//   OUTPUTS: { bool }
//   SIDE_EFFECTS: none
// END_CONTRACT: is_stale
pub fn is_stale(mtime_secs: u64, now_secs: u64, ttl: u64) -> bool {
    now_secs.saturating_sub(mtime_secs) > ttl
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn file_mtime_secs(p: &std::path::Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// START_CONTRACT: list_ext
//   PURPOSE: Прочитать активные сигналы заданного расширения (signal|busy); ttl=Some -> отфильтровать зависшие по mtime.
//   INPUTS: { ext: &str - расширение без точки; ttl: Option<u64> - порог staleness (None = без фильтра) }
//   OUTPUTS: { Vec<Signal> - путь, ключ (basename lower), полный cwd (lower) }
//   SIDE_EFFECTS: чтение каталога signals
// END_CONTRACT: list_ext
fn list_ext(ext: &str, ttl: Option<u64>) -> Vec<Signal> {
    let dir = match signal_dir() {
        Some(d) => d,
        None => return Vec::new(),
    };
    let now = now_secs();
    let mut out = Vec::new();
    // START_BLOCK_SCAN_SIGNALS
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case(ext)) != Some(true) {
                continue;
            }
            if let Some(ttl) = ttl {
                if is_stale(file_mtime_secs(&p), now, ttl) {
                    continue; // зависший .busy -> пропускаем
                }
            }
            let content = match std::fs::read_to_string(&p) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let cwd = match parse_signal(&content) {
                Some(c) => c,
                None => continue,
            };
            let key = project_key(&cwd);
            if key.is_empty() {
                continue;
            }
            let agent = parse_agent(&content);
            out.push(Signal { path: p, key, cwd: cwd.to_lowercase(), agent });
        }
    }
    // END_BLOCK_SCAN_SIGNALS
    out
}

// Активные .signal (звоночек) — без staleness-фильтра.
pub fn list_signals() -> Vec<Signal> {
    list_ext("signal", None)
}

// Активные .busy (индикатор работы) — с фильтром зависших по mtime.
fn list_busy() -> Vec<Signal> {
    list_ext("busy", Some(BUSY_STALE_SECS))
}

// START_CONTRACT: bell_keys
//   PURPOSE: Множество «звенящих» ключей проектов для подсветки в paint.
//   INPUTS: {}
//   OUTPUTS: { HashSet<String> - ключи проектов (lower) с активным сигналом }
//   SIDE_EFFECTS: чтение каталога signals
// END_CONTRACT: bell_keys
pub fn bell_keys() -> HashSet<String> {
    list_signals().into_iter().map(|s| s.key).collect()
}

// START_CONTRACT: bell_cwds
//   PURPOSE: Множество полных cwd (lower) активных сигналов — для точной подсветки окна по пути (Phase-15).
//   INPUTS: {}
//   OUTPUTS: { HashSet<String> - cwd проектов (lower) с активным сигналом }
//   SIDE_EFFECTS: чтение каталога signals
// END_CONTRACT: bell_cwds
pub fn bell_cwds() -> HashSet<String> {
    list_signals().into_iter().map(|s| s.cwd).collect()
}

// START_CONTRACT: busy_keys
//   PURPOSE: Множество basename-ключей (lower) проектов с активным .busy — индикатор работы (fallback) — Phase-17.
//   INPUTS: {}
//   OUTPUTS: { HashSet<String> }
//   SIDE_EFFECTS: чтение каталога signals
// END_CONTRACT: busy_keys
pub fn busy_keys() -> HashSet<String> {
    list_busy().into_iter().map(|s| s.key).collect()
}

// START_CONTRACT: busy_cwds
//   PURPOSE: Множество полных cwd (lower) с активным .busy (staleness-фильтр) — бегущие точки по пути — Phase-17.
//   INPUTS: {}
//   OUTPUTS: { HashSet<String> }
//   SIDE_EFFECTS: чтение каталога signals
// END_CONTRACT: busy_cwds
pub fn busy_cwds() -> HashSet<String> {
    list_busy().into_iter().map(|s| s.cwd).collect()
}

// START_CONTRACT: sessions
//   PURPOSE: Живые сессии-агенты по session-id: .alive (присутствие + agent) + состояние из .busy/.signal.
//   INPUTS: {}
//   OUTPUTS: { Vec<Sess> - по одной на sid (alive|busy|signal); состояние busy > signal > idle }
//   SIDE_EFFECTS: чтение каталога signals
//   LINKS: M-RENDER (квадраты-статусы по агенту/состоянию), M-SIGNAL
// END_CONTRACT: sessions
pub fn sessions() -> Vec<Sess> {
    use std::collections::HashMap;
    let sid = |s: &Signal| s.path.file_stem().and_then(|x| x.to_str()).unwrap_or("").to_string();
    let mut m: HashMap<String, Sess> = HashMap::new();
    let mut put = |s: Signal, st: SessState, force: bool| {
        let id = sid(&s);
        let e = m.entry(id.clone()).or_insert(Sess {
            sid: id,
            cwd: s.cwd.clone(),
            key: s.key.clone(),
            agent: String::new(),
            state: SessState::Idle,
        });
        if e.agent.is_empty() && !s.agent.is_empty() {
            e.agent = s.agent.clone();
        }
        if force {
            e.state = st; // busy перебивает всё
        } else if e.state == SessState::Idle {
            e.state = st; // signal поднимает idle -> done (busy позже перебьёт)
        }
    };
    for s in list_ext("alive", Some(ALIVE_STALE_SECS)) {
        put(s, SessState::Idle, false); // D-16: TTL против вечного фантома после краха
    }
    for s in list_ext("signal", None) {
        put(s, SessState::Done, false);
    }
    for s in list_ext("busy", Some(BUSY_STALE_SECS)) {
        put(s, SessState::Working, true);
    }
    let mut out: Vec<Sess> = m.into_values().collect();
    sort_sessions(&mut out); // детерминированный порядок: HashMap иначе тасует квадраты каждый опрос
    out
}

// Порядок агента в ряду квадратов: Claude слева (0), Kimi (1), прочие (2).
fn agent_rank(agent: &str) -> u8 {
    match agent {
        "claude" => 0,
        "kimi" => 1,
        _ => 2,
    }
}

// START_CONTRACT: sort_sessions
//   PURPOSE: Детерминированный порядок квадратов на строке: Claude слева, Kimi справа, прочие дальше;
//            внутри агента — по sid (стабильно между опросами). Без этого HashMap-порядок тасует квадраты.
//   INPUTS: { v: &mut [Sess] }
//   OUTPUTS: { () - сортирует на месте }
//   SIDE_EFFECTS: none
//   LINKS: M-RENDER (порядок отрисовки квадратов)
// END_CONTRACT: sort_sessions
pub fn sort_sessions(v: &mut [Sess]) {
    v.sort_by(|a, b| agent_rank(&a.agent).cmp(&agent_rank(&b.agent)).then_with(|| a.sid.cmp(&b.sid)));
}

// START_CONTRACT: sess_matches_row
//   PURPOSE: Чистое — относится ли сессия к строке окна (путь через path_within, иначе basename).
//   INPUTS: { cwd: &str; key: &str; row_path: Option<&str>; row_name: &str }
//   OUTPUTS: { bool }
//   SIDE_EFFECTS: none
// END_CONTRACT: sess_matches_row
pub fn sess_matches_row(cwd: &str, key: &str, row_path: Option<&str>, row_name: &str) -> bool {
    match row_path {
        Some(p) => path_within(cwd, p),
        None => key == row_name.to_lowercase(),
    }
}

// START_CONTRACT: reconcile
//   PURPOSE: Удалить .signal, чьё окно проекта сейчас на переднем плане (сброс по фокусу).
//   INPUTS: { items: &[WinItem] - открытые окна; fg: HWND - окно в фокусе }
//   OUTPUTS: { () }
//   SIDE_EFFECTS: удаляет файлы-сигналы
// END_CONTRACT: reconcile
pub fn reconcile(items: &[WinItem], fg: HWND) {
    let Some(fgw) = items.iter().find(|it| it.hwnd == fg) else { return };
    let fg_path = fgw.path.as_deref();
    let fg_key = fgw.name.to_lowercase();
    // START_BLOCK_CLEAR_FOCUSED
    for s in list_signals() {
        if should_clear(&s.cwd, &s.key, fg_path, &fg_key) {
            let _ = std::fs::remove_file(&s.path);
        }
    }
    // END_BLOCK_CLEAR_FOCUSED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_signal_takes_cwd_or_first_line() {
        assert_eq!(parse_signal("d:\\Python\\claudebar\n"), Some("d:\\Python\\claudebar".to_string()));
        assert_eq!(parse_signal("cwd=d:\\X\ntime=123"), Some("d:\\X".to_string()));
        assert_eq!(parse_signal("\n\n  \n"), None);
        assert_eq!(parse_signal(""), None);
    }

    #[test]
    fn project_key_is_basename_lower() {
        assert_eq!(project_key("d:\\Python\\claudebar"), "claudebar");
        assert_eq!(project_key("d:/Python/ConstructMan/"), "constructman");
        assert_eq!(project_key("C:\\ws\\My Proj"), "my proj");
    }

    #[test]
    fn should_clear_by_full_path() {
        // точный матч по полному пути: одноимённые в разных путях НЕ путаются
        assert!(should_clear("d:\\a\\claudebar", "claudebar", Some("D:\\a\\claudebar"), "claudebar")); // регистронезависимо
        assert!(!should_clear("d:\\a\\claudebar", "claudebar", Some("E:\\b\\claudebar"), "claudebar")); // другой путь -> НЕ гасит
        // окно без пути -> fallback на basename
        assert!(should_clear("d:\\a\\claudebar", "claudebar", None, "claudebar"));
        assert!(!should_clear("d:\\a\\claudebar", "claudebar", None, "other"));
        // фокус окна-родителя гасит сигнал сессии в подпапке (cwd вложен в путь окна)
        assert!(should_clear("d:\\python\\mosco\\doc", "doc", Some("D:\\Python\\Mosco"), "mosco"));
    }

    #[test]
    fn row_signaled_matches_session_in_subfolder() {
        // regression: при FULLPATHS (путь в заголовке) точки/звоночек пропали у проекта,
        // где Claude-сессия запущена в ПОДПАПКЕ открытого проекта (окно D:\Python\Mosco, cwd D:\Python\Mosco\doc).
        let cwds: HashSet<String> = ["d:\\python\\mosco\\doc".to_string()].into_iter().collect();
        let keys: HashSet<String> = ["doc".to_string()].into_iter().collect();
        // путь окна = корень проекта, cwd сессии вложен -> МАТЧ (точный contains не находил)
        assert!(row_signaled(Some("D:\\Python\\Mosco"), "Mosco", &cwds, &keys));
        // точное совпадение пути всё ещё матчит
        assert!(row_signaled(Some("D:\\Python\\Mosco\\doc"), "doc", &cwds, &keys));
        // D-06: одноимённый проект в ДРУГОМ пути НЕ зажигается
        assert!(!row_signaled(Some("D:\\Other\\Mosco"), "Mosco", &cwds, &keys));
        // префикс по имени, не по границе сегмента, НЕ матчит (D:\Python\Mos != ...\Mosco\doc)
        assert!(!row_signaled(Some("D:\\Python\\Mos"), "Mos", &cwds, &keys));
        // без пути у строки -> fallback на basename
        assert!(row_signaled(None, "doc", &cwds, &keys));
        assert!(!row_signaled(None, "mosco", &cwds, &keys));
    }

    #[test]
    fn parse_agent_and_sess_matches_row() {
        // agent= парсится, регистронезависимо; parse_signal игнорирует agent=-строку
        assert_eq!(parse_agent("cwd=d:\\x\nagent=Kimi"), "kimi");
        assert_eq!(parse_agent("d:\\x"), ""); // нет agent=
        assert_eq!(parse_signal("agent=kimi\ncwd=d:\\x"), Some("d:\\x".to_string()));
        // матч сессии к строке: слэши нормализуются, D-06, basename-fallback
        assert!(sess_matches_row("d:/python/claudebar", "claudebar", Some("D:\\Python\\claudebar"), "claudebar"));
        assert!(!sess_matches_row("d:\\other\\claudebar", "claudebar", Some("D:\\Python\\claudebar"), "claudebar"));
        assert!(sess_matches_row("d:\\x", "claudebar", None, "ClaudeBar")); // basename, регистронезависимо
    }

    #[test]
    fn sort_sessions_claude_left_kimi_right() {
        let mk = |sid: &str, agent: &str| Sess {
            sid: sid.into(),
            cwd: "c".into(),
            key: "k".into(),
            agent: agent.into(),
            state: SessState::Idle,
        };
        let mut v = vec![mk("z", "kimi"), mk("a", "claude"), mk("b", "kimi"), mk("q", "")];
        sort_sessions(&mut v);
        let agents: Vec<&str> = v.iter().map(|s| s.agent.as_str()).collect();
        assert_eq!(agents, vec!["claude", "kimi", "kimi", ""]); // Claude слева, прочий в конце
        assert_eq!(v[1].sid, "b"); // внутри Kimi по sid стабильно: b < z
        assert_eq!(v[2].sid, "z");
    }

    #[test]
    fn is_stale_by_mtime() {
        assert!(!is_stale(1000, 1500, 600)); // 500с < 600 -> свежий
        assert!(is_stale(1000, 1700, 600)); // 700с > 600 -> устарел
        assert!(!is_stale(1700, 1000, 600)); // mtime в будущем -> не устарел (saturating)
        assert!(!is_stale(1000, 1600, 600)); // ровно 600 -> ещё не устарел (строго >)
    }
}
