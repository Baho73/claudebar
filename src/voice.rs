// FILE: src/voice.rs
// VERSION: 1.4.0
// START_MODULE_CONTRACT
//   PURPOSE: Оркестрация голосового ввода: стейт-машина idle->recording->transcribing, спавн worker-потока распознавания, доставка текста в UI.
//   SCOPE: VoiceState/VoiceEvent + чистая next_state; Voice (toggle: старт/стоп записи + спавн worker stt->transform; on_done; state; set_always_on). Worker шлёт текст в UI через PostMessage(WM_APP_VOICE_DONE).
//   DEPENDS: M-AUDIO (захват), M-STT (распознавание), M-TRANSFORM (чистка/словарь), M-CONFIG (параметры)
//   LINKS: M-VOICE
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   VoiceState           - Idle | Recording | Transcribing
//   VoiceEvent           - Toggle | Done
//   next_state           - чистое: (состояние, событие) -> следующее состояние
//   WM_APP_VOICE_DONE    - оконное сообщение: worker -> UI (lparam = id результата в реестре, не сырой указатель — audit #3)
//   stash_result/take_result - реестр результатов worker->UI по id (замена Box::into_raw в lparam)
//   Voice                - держатель состояния + активного Recorder (legacy) или персистентного Mic (always-on)
//   Voice::toggle        - переключатель записи/распознавания (always-on -> arm/disarm Mic, иначе Recorder cold-start)
//   Voice::set_always_on - старт/дроп персистентного Mic по галочке (только на Idle)
//   Voice::on_done       - вернуть в Idle после доставки текста
//   Voice::poll          - авто-стоп по тишине (в записи) + watchdog зависшей Transcribing -> Idle (D-13)
//   Voice::level         - текущий уровень микрофона активного источника (для индикатора-полосы)
//   cue_start/cue_end    - короткие тоны старта/конца диктовки (Beep, в фоне)
//   vlog                 - диагностический лог голосового ввода (%APPDATA%\claudebar\voice.log)
//   Voice::stream_tick   - стрим-заход по таймеру: срез окна -> фон transcribe_segments -> WM_APP_STREAM_PARTIAL (Phase-24)
//   Voice::on_partial    - приём сегментов: committed_segs -> фикс + сдвиг окна по end-тайму (Phase-24)
//   WM_APP_STREAM_PARTIAL - оконное сообщение стрим-worker -> UI (lparam = id сегментов в реестре) — Phase-24
//   stash_partial/take_partial - реестр сегментов стрим-захода по id
//   StreamState          - состояние стриминга: committed_text, window_start(сэмплы), prev_texts, rate
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.4.0 - Phase-24 step-5 (стриминг, вариант A): StreamState + stream_tick (срез окна [window_start..now] -> фон transcribe_segments -> WM_APP_STREAM_PARTIAL, троттлинг STREAM_TICK_SECS, без наложения) + on_partial (committed_segs LocalAgreement-2 -> committed_text + сдвиг window_start по end-тайму сегмента × rate). stop_to_transcribe: при streaming отдаёт хвост+committed (prefix) в spawn_worker (merge_committed+process на стопе, разом). Реестр stash_partial/take_partial. Только при voice_streaming; иначе legacy. Сброс stream в on_done/watchdog.
//   v1.3.0 - fix(grace-fix, FPF D-13/D-14): (D-13) воркер оборачивает stt/transform в catch_unwind — паника (panic=unwind) не оставляет UI в Transcribing, текст постится всегда; watchdog в poll (transcribe_start + TRANSCRIBE_TIMEOUT_SECS=150) возвращает зависшую Transcribing в Idle. (D-14) set_always_on -> bool: тоггл «микрофон всегда вкл» во время записи не применяется и НЕ меняет конфиг (ini/галочка не разъезжаются с реальностью).
//   v1.2.1 - fix(grace-fix, audit #3): worker->UI отдаёт текст через id-реестр (stash_result/take_result), а не Box::into_raw в lparam. Раньше обработчик WM_APP_VOICE_DONE безусловно разыменовывал lparam как *mut String -> любой процесс мог послать мусор -> UB/порча кучи. Теперь мусорный id -> None. Тест stash_take_result_roundtrip_and_unknown_none.
//   v1.2.0 - Phase-22: always-on микрофон + pre-roll. Voice держит Option<Mic>; set_always_on(on) стартует/дропает персистентный Mic (только на Idle). toggle/stop/poll/level ветвятся: при Mic -> arm/disarm_take (тёплый поток, pre-roll, первое слово не теряется), иначе legacy Recorder (cold-start). Галочка деф. ВЫКЛ (M-CONFIG voice_always_on).
//   v1.1.0 - Phase-19 доводка: авто-стоп по тишине (poll: 2с после речи / 8с без речи), короткие тоны старта/конца (Beep, в фоне), уровень микрофона (level), диагностический лог (vlog в voice.log). Подтверждено рабочим; валил поведенческий AV (Касперский), не баг.
//   v1.0.0 - Phase-19 step-1: стейт-машина голосового ввода. Recorder (!Send) живёт на UI-потоке
//                между нажатиями; распознавание (M-STT) + чистка (M-TRANSFORM) — в worker-потоке, результат
//                в UI через PostMessage (lparam = Box<String>). HWND передаём в поток как isize.
// END_CHANGE_SUMMARY

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Diagnostics::Debug::Beep;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

// Сообщение «распознавание готово»: lparam = id результата в реестре (НЕ сырой указатель —
// чужое сообщение WM_APP+2 с мусором даёт None, без разыменования). WM_APP+1 занят поиском. (audit #3)
pub const WM_APP_VOICE_DONE: u32 = WM_APP + 2;

// Реестр готовых результатов worker->UI: worker кладёт текст под id, UI забирает по id.
// Заменяет Box::into_raw в lparam (любой процесс мог послать WM_APP+2 с мусорным указателем -> UB/крах).
static NEXT_RESULT_ID: AtomicU64 = AtomicU64::new(1);
fn results() -> &'static Mutex<HashMap<u64, String>> {
    static R: OnceLock<Mutex<HashMap<u64, String>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

// START_CONTRACT: stash_result
//   PURPOSE: Положить готовый текст в реестр, вернуть id (для передачи в lparam PostMessage).
//   INPUTS: { text: String }
//   OUTPUTS: { u64 - id (всегда > 0) }
//   SIDE_EFFECTS: вставка в глобальный реестр результатов
// END_CONTRACT: stash_result
pub fn stash_result(text: String) -> u64 {
    let id = NEXT_RESULT_ID.fetch_add(1, Ordering::Relaxed);
    results().lock().unwrap_or_else(|e| e.into_inner()).insert(id, text);
    id
}

// START_CONTRACT: take_result
//   PURPOSE: Забрать текст по id (удалив из реестра). Неизвестный id (мусор из чужого сообщения) -> None.
//   INPUTS: { id: u64 }
//   OUTPUTS: { Option<String> }
//   SIDE_EFFECTS: удаление из реестра
// END_CONTRACT: take_result
pub fn take_result(id: u64) -> Option<String> {
    results().lock().unwrap_or_else(|e| e.into_inner()).remove(&id)
}

// Сообщение «частичное распознавание готово» (стрим-заход): lparam = id сегментов в реестре ниже (Phase-24).
pub const WM_APP_STREAM_PARTIAL: u32 = WM_APP + 3;

// Реестр частичных результатов стрим-захода (Vec<Segment>) worker->UI по id — как результаты, но сегменты.
static NEXT_PARTIAL_ID: AtomicU64 = AtomicU64::new(1);
fn partials() -> &'static Mutex<HashMap<u64, Vec<crate::stt::Segment>>> {
    static P: OnceLock<Mutex<HashMap<u64, Vec<crate::stt::Segment>>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}
pub fn stash_partial(segs: Vec<crate::stt::Segment>) -> u64 {
    let id = NEXT_PARTIAL_ID.fetch_add(1, Ordering::Relaxed);
    partials().lock().unwrap_or_else(|e| e.into_inner()).insert(id, segs);
    id
}
pub fn take_partial(id: u64) -> Option<Vec<crate::stt::Segment>> {
    partials().lock().unwrap_or_else(|e| e.into_inner()).remove(&id)
}

// Авто-стоп записи по молчанию (чтобы не писать часами, если забыл выключить).
const SILENCE_STOP_SECS: f32 = 2.0; // была речь -> стоп после стольких секунд тишины
const NO_SPEECH_CAP_SECS: f32 = 8.0; // речи вообще не было -> стоп через столько
const TRANSCRIBE_TIMEOUT_SECS: f32 = 150.0; // watchdog: зависшая Transcribing (паника воркера/отказ Post) -> Idle (D-13)
const STREAM_TICK_SECS: f32 = 3.0; // не чаще этого — стрим-заход (Phase-24)
const STREAM_MARGIN_SEGS: usize = 1; // держать последний сегмент незафиксированным (плавающий хвост)
const STREAM_MIN_NEW_SECS: f32 = 1.0; // не гонять заход, если нового аудио меньше секунды

// Короткие тоны старта/конца диктовки (70мс; старт выше, конец ниже). В фоне — Beep блокирует на длительность.
pub fn cue_start() {
    std::thread::spawn(|| unsafe {
        let _ = Beep(1200, 70);
    });
}
pub fn cue_end() {
    std::thread::spawn(|| unsafe {
        let _ = Beep(760, 70);
    });
}

// Диагностический лог голосового ввода в %APPDATA%\claudebar\voice.log (у exe нет консоли).
pub fn vlog(msg: &str) {
    use std::io::Write;
    let dir = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join("claudebar");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("voice.log")) {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{t}] {msg}");
    }
}

// START_BLOCK_HEALTH (Phase-27): фоновая проверка «сервер whisper поднят».
// Опрос делает TCP-connect (блокирует до таймаута) — только в отдельном потоке, никогда на UI.
// Стартовое значение true: пока первый опрос не прошёл, не мигаем красным баннером на ровном месте.
static WHISPER_OK: AtomicBool = AtomicBool::new(true);
static HEALTH_INFLIGHT: AtomicBool = AtomicBool::new(false); // не копим потоки, если сервер тупит

const HEALTH_TIMEOUT_MS: u64 = 1200; // localhost: живой сервер отвечает за миллисекунды

// START_CONTRACT: whisper_ok
//   PURPOSE: Последний известный статус сервера whisper (для баннера в M-RENDER).
//   INPUTS: { }
//   OUTPUTS: { bool - true = /health отвечал 200 на последнем опросе (или опроса ещё не было) }
//   SIDE_EFFECTS: none (чтение атомика)
// END_CONTRACT: whisper_ok
pub fn whisper_ok() -> bool {
    WHISPER_OK.load(Ordering::Relaxed)
}

// START_CONTRACT: spawn_health_check
//   PURPOSE: Опросить /health в фоновом потоке и обновить статус; при смене статуса — перерисовать панель.
//   INPUTS: { hwnd: HWND - окно панели (repaint при смене); url: String - whisper_url из конфига }
//   OUTPUTS: { () }
//   SIDE_EFFECTS: спавн потока, TCP GET /health, запись WHISPER_OK, PostMessage(WM_APP_HEALTH) при смене
//   LINKS: M-VOICE, M-STT (is_alive), M-RENDER (баннер)
// END_CONTRACT: spawn_health_check
pub fn spawn_health_check(hwnd: HWND, url: String) {
    // предыдущий опрос ещё висит (сервер не отвечает) — второй поток не нужен
    if HEALTH_INFLIGHT.swap(true, Ordering::SeqCst) {
        return;
    }
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        let ok = crate::stt::is_alive(&url, HEALTH_TIMEOUT_MS);
        let was = WHISPER_OK.swap(ok, Ordering::Relaxed);
        HEALTH_INFLIGHT.store(false, Ordering::SeqCst);
        if was != ok {
            vlog(if ok { "health: whisper поднялся" } else { "health: whisper НЕ отвечает" });
            unsafe {
                let _ = PostMessageW(HWND(h as *mut core::ffi::c_void), WM_APP_HEALTH, WPARAM(0), LPARAM(0));
            }
        }
    });
}

// Сообщение «статус whisper сменился» -> UI перерисовывает баннер (WM_APP+1..+3 заняты).
pub const WM_APP_HEALTH: u32 = WM_APP + 4;
// END_BLOCK_HEALTH

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VoiceState {
    Idle,
    Recording,
    Transcribing,
}

#[derive(Clone, Copy, Debug)]
pub enum VoiceEvent {
    Toggle,
    Done,
}

// START_CONTRACT: next_state
//   PURPOSE: Чистый переход стейт-машины голосового ввода.
//   INPUTS: { cur: VoiceState, ev: VoiceEvent }
//   OUTPUTS: { VoiceState - следующее состояние }
//   SIDE_EFFECTS: none
// END_CONTRACT: next_state
pub fn next_state(cur: VoiceState, ev: VoiceEvent) -> VoiceState {
    use VoiceEvent::*;
    use VoiceState::*;
    match (cur, ev) {
        (Idle, Toggle) => Recording,
        (Recording, Toggle) => Transcribing,
        (Transcribing, Toggle) => Transcribing, // занято распознаванием — игнор
        (_, Done) => Idle,
    }
}

// START_CONTRACT: Voice
//   PURPOSE: Состояние голосового ввода + активный Recorder (захват между нажатиями хоткея).
//   INPUTS: { state, rec: Option<Recorder> }
//   OUTPUTS: { state() для индикатора; toggle/on_done — переходы }
//   SIDE_EFFECTS: владеет аудио-потоком в Recording
//   LINKS: M-MAIN (владелец в App, дёргает по WM_HOTKEY/WM_APP_VOICE_DONE)
// END_CONTRACT: Voice
// Состояние стриминг-диктовки (Phase-24 вариант A): накопленный committed + скользящее окно по сэмплам.
struct StreamState {
    committed_text: String,  // уже зафиксированный текст (сырой, из сегментов)
    window_start: usize,     // сэмпл-начало текущего окна (сдвигается по end-тайму сегмента)
    prev_texts: Vec<String>, // тексты сегментов прошлого захода (LocalAgreement-2)
    rate: u32,               // частота источника (сэмплов/с)
    last_tick: Instant,      // время последнего захода (троттлинг)
    in_flight: bool,         // заход в фоне -> не пускать второй
}

pub struct Voice {
    state: VoiceState,
    rec: Option<crate::audio::Recorder>, // разовый захват (always-on ВЫКЛ): поток создаётся/дропается на запись
    mic: Option<crate::audio::Mic>, // персистентный always-on захват (always-on ВКЛ) — Phase-22
    transcribe_start: Option<Instant>, // когда вошли в Transcribing — watchdog зависшей транскрибации (D-13)
    stream: Option<StreamState>, // стриминг-диктовка (Phase-24), None вне стриминга
}

impl Default for Voice {
    fn default() -> Self {
        Voice { state: VoiceState::Idle, rec: None, mic: None, transcribe_start: None, stream: None }
    }
}

impl Voice {
    pub fn new() -> Self {
        Voice::default()
    }

    pub fn state(&self) -> VoiceState {
        self.state
    }

    // Текущий уровень микрофона 0.0..1.0 (только во время записи) — для индикатора громкости.
    pub fn level(&self) -> f32 {
        if self.state != VoiceState::Recording {
            return 0.0;
        }
        if let Some(mic) = &self.mic {
            mic.level()
        } else if let Some(rec) = &self.rec {
            rec.level()
        } else {
            0.0
        }
    }

    // START_BLOCK_SET_ALWAYS_ON
    // Включить/выключить always-on микрофон по галочке. Применяется только на Idle (risk-43):
    // ВКЛ -> запустить персистентный Mic (поток крутится постоянно, кольцо pre-roll);
    // ВЫКЛ -> дропнуть Mic (поток закрыт, индикатор микрофона ОС гаснет, toggle падает на legacy Recorder).
    // Возвращает true, если флаг применён (только на Idle). false -> вызывающий НЕ меняет конфиг (D-14),
    // чтобы галочка/ini не разъезжались с реальностью при тоггле во время записи.
    pub fn set_always_on(&mut self, on: bool) -> bool {
        if self.state != VoiceState::Idle {
            vlog("set_always_on: не на Idle — не применено (D-14)");
            return false;
        }
        if on && self.mic.is_none() {
            match crate::audio::start_persistent() {
                Ok(m) => {
                    self.mic = Some(m);
                    vlog("set_always_on: persistent Mic запущен (always-on + pre-roll)");
                }
                Err(e) => vlog(&format!("set_always_on: start_persistent FAILED: {e}")),
            }
        } else if !on && self.mic.is_some() {
            self.mic = None; // drop -> закрытие потока, индикатор гаснет
            vlog("set_always_on: persistent Mic остановлен (legacy cold-start)");
        }
        true
    }
    // END_BLOCK_SET_ALWAYS_ON

    // Переключатель по хоткею: старт записи / стоп+распознавание / игнор (занято).
    // Состояние меняем через next_state (единый источник переходов), но только при успехе side-effect.
    pub fn toggle(&mut self, hwnd: HWND, cfg: &crate::config::Config) {
        match self.state {
            VoiceState::Idle => {
                // START_BLOCK_START_REC
                if let Some(mic) = &self.mic {
                    // always-on: поток уже тёплый, кольцо держит pre-roll -> arm мгновенно, первое слово не теряется
                    mic.arm();
                    self.state = next_state(self.state, VoiceEvent::Toggle); // -> Recording
                    vlog("toggle: Idle -> Recording (always-on arm, pre-roll)");
                    cue_start(); // звук «слушаю»
                } else {
                    match crate::audio::start_recording() {
                        Ok(r) => {
                            self.rec = Some(r);
                            self.state = next_state(self.state, VoiceEvent::Toggle); // -> Recording
                            vlog("toggle: Idle -> Recording (start_recording ok)");
                            cue_start(); // звук «слушаю»
                        }
                        Err(e) => vlog(&format!("toggle: start_recording FAILED: {e}")),
                    }
                }
                // END_BLOCK_START_REC
            }
            VoiceState::Recording => self.stop_to_transcribe(hwnd, cfg, "хоткей"),
            VoiceState::Transcribing => vlog("toggle: занято (Transcribing) — игнор"),
        }
    }

    // Активный источник записи: аксессоры (Mic приоритетен над разовым Recorder) — Phase-24.
    fn src_rate(&self) -> Option<u32> {
        if let Some(m) = &self.mic {
            Some(m.rate())
        } else {
            self.rec.as_ref().map(|r| r.rate())
        }
    }
    fn src_samples_len(&self) -> Option<usize> {
        if let Some(m) = &self.mic {
            Some(m.samples_len())
        } else {
            self.rec.as_ref().map(|r| r.samples_len())
        }
    }
    fn src_snapshot_from(&self, start: usize) -> Option<Vec<u8>> {
        if let Some(m) = &self.mic {
            Some(m.snapshot_from(start))
        } else {
            self.rec.as_ref().map(|r| r.snapshot_from(start))
        }
    }
    // Завершить запись, вернуть полный WAV (Mic disarm / Recorder stop); None если источника нет.
    fn stop_source(&mut self) -> Option<Vec<u8>> {
        if let Some(m) = &self.mic {
            Some(m.disarm_take())
        } else {
            self.rec.take().map(|r| r.stop())
        }
    }

    // START_BLOCK_STOP_REC
    // Остановить запись и запустить распознавание (по хоткею или авто-стопу по тишине).
    fn stop_to_transcribe(&mut self, hwnd: HWND, cfg: &crate::config::Config, why: &str) {
        // стриминг (Phase-24): хвост от window_start + накопленный committed; иначе весь WAV, без префикса.
        let (wav, prefix) = if cfg.voice_streaming && self.stream.is_some() {
            let st = self.stream.take().unwrap();
            let tail = self.src_snapshot_from(st.window_start).unwrap_or_default();
            let _ = self.stop_source(); // завершить запись; полный WAV не нужен (есть committed + tail)
            (tail, st.committed_text)
        } else {
            self.stream = None;
            match self.stop_source() {
                Some(w) => (w, String::new()),
                None => {
                    self.state = VoiceState::Idle;
                    vlog("stop: нет источника записи -> Idle");
                    return;
                }
            }
        };
        self.state = next_state(self.state, VoiceEvent::Toggle); // -> Transcribing
        self.transcribe_start = Some(Instant::now()); // watchdog отсчёт (D-13)
        vlog(&format!("stop ({why}): -> Transcribing (wav {} байт, committed {} симв)", wav.len(), prefix.chars().count()));
        self.spawn_worker(hwnd, cfg, wav, prefix);
    }
    // END_BLOCK_STOP_REC

    // START_BLOCK_POLL_SILENCE
    // Авто-стоп по тишине: вызывается из таймера, пока идёт запись. true = состояние изменилось.
    pub fn poll(&mut self, hwnd: HWND, cfg: &crate::config::Config) -> bool {
        // watchdog зависшей транскрибации (паника воркера / отказ PostMessage) -> Idle (D-13)
        if self.state == VoiceState::Transcribing {
            if self.transcribe_start.map(|t| t.elapsed().as_secs_f32() >= TRANSCRIBE_TIMEOUT_SECS).unwrap_or(false) {
                vlog("poll: Transcribing watchdog timeout -> Idle");
                self.state = VoiceState::Idle;
                self.transcribe_start = None;
                self.stream = None; // сброс стриминга (Phase-24)
                return true;
            }
            return false;
        }
        if self.state != VoiceState::Recording {
            return false;
        }
        // (had_speech, trailing_silence, duration) активного источника записи
        let m = if let Some(mic) = &self.mic {
            Some((mic.had_speech(), mic.trailing_silence(), mic.duration()))
        } else {
            self.rec.as_ref().map(|rec| (rec.had_speech(), rec.trailing_silence(), rec.duration()))
        };
        let stop = match m {
            Some((had, trailing, dur)) => {
                (had && trailing >= SILENCE_STOP_SECS) || (!had && dur >= NO_SPEECH_CAP_SECS)
            }
            None => false,
        };
        if stop {
            self.stop_to_transcribe(hwnd, cfg, "тишина");
            return true;
        }
        false
    }
    // END_BLOCK_POLL_SILENCE

    // Вернуть в Idle (после доставки текста в UI и вставки).
    pub fn on_done(&mut self) {
        self.state = next_state(self.state, VoiceEvent::Done);
        self.transcribe_start = None; // watchdog сброшен (D-13)
        self.stream = None; // сброс стриминга к следующей диктовке (Phase-24)
    }

    // START_BLOCK_SPAWN_WORKER
    // Распознать WAV в фоне (M-STT -> M-TRANSFORM) и отдать текст в UI через PostMessage.
    // prefix — уже зафиксированный стриминг-текст (Phase-24); wav — хвост (стрим) или весь WAV (legacy, prefix="").
    fn spawn_worker(&self, hwnd: HWND, cfg: &crate::config::Config, wav: Vec<u8>, prefix: String) {
        let url = cfg.whisper_url.clone();
        let lang = cfg.voice_language.clone();
        let hot = cfg.hotwords.clone();
        let prompt = cfg.initial_prompt.clone();
        let vocab = crate::config::parse_vocab(&cfg.vocab);
        let trail = cfg.voice_trailing_space; // хвостовой пробел после фразы — Phase-23
        let hwnd_i = hwnd.0 as isize; // HWND !Send -> переносим как isize
        vlog(&format!("worker: POST {url} (wav {} байт, lang={lang}, prefix {} симв)", wav.len(), prefix.chars().count()));
        std::thread::spawn(move || {
            // D-13: паника stt/transform (panic=unwind) не должна молча убить воркер —
            // ловим, но ВСЕГДА постим (иначе UI застрянет в Transcribing). При панике отдаём хотя бы committed.
            let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let tail = match crate::stt::transcribe(&url, &wav, &lang, &hot, &prompt) {
                    Ok(t) => t,
                    Err(e) => {
                        vlog(&format!("worker: STT FAILED: {e}"));
                        String::new()
                    }
                };
                let full = crate::transform::merge_committed(&prefix, &tail); // prefix="" -> просто tail (legacy)
                let out = crate::transform::process(&full, &vocab, trail);
                vlog(&format!("worker: tail={:?} -> out={:?}", tail, out));
                out
            }))
            .unwrap_or_else(|_| {
                vlog("worker: PANIC перехвачена -> committed без хвоста (D-13)");
                crate::transform::process(&prefix, &vocab, trail)
            });
            let id = stash_result(text); // реестр id вместо сырого указателя в lparam (audit #3)
            unsafe {
                let _ = PostMessageW(
                    HWND(hwnd_i as *mut core::ffi::c_void),
                    WM_APP_VOICE_DONE,
                    WPARAM(0),
                    LPARAM(id as isize),
                );
            }
        });
    }
    // END_BLOCK_SPAWN_WORKER

    // START_BLOCK_STREAM
    // Стрим-заход (Phase-24 вариант A): из таймера при Recording+voice_streaming. Снимает срез окна
    // [window_start..now], в фоне распознаёт с сегментами, шлёт WM_APP_STREAM_PARTIAL. Троттлинг + без наложения.
    pub fn stream_tick(&mut self, hwnd: HWND, cfg: &crate::config::Config) {
        if self.state != VoiceState::Recording || !cfg.voice_streaming {
            return;
        }
        let Some(rate) = self.src_rate() else { return };
        if self.stream.is_none() {
            // первый тик только инициализирует состояние (дать аудио накопиться)
            self.stream = Some(StreamState {
                committed_text: String::new(),
                window_start: 0,
                prev_texts: Vec::new(),
                rate,
                last_tick: Instant::now(),
                in_flight: false,
            });
            return;
        }
        let (in_flight, elapsed, window_start) = {
            let st = self.stream.as_ref().unwrap();
            (st.in_flight, st.last_tick.elapsed().as_secs_f32(), st.window_start)
        };
        if in_flight || elapsed < STREAM_TICK_SECS {
            return;
        }
        let Some(total) = self.src_samples_len() else { return };
        if ((total.saturating_sub(window_start)) as f32) < STREAM_MIN_NEW_SECS * rate as f32 {
            return; // мало нового аудио с прошлого захода
        }
        let Some(wav) = self.src_snapshot_from(window_start) else { return };
        if let Some(st) = self.stream.as_mut() {
            st.in_flight = true;
            st.last_tick = Instant::now();
        }
        let (url, lang, hot, prompt) =
            (cfg.whisper_url.clone(), cfg.voice_language.clone(), cfg.hotwords.clone(), cfg.initial_prompt.clone());
        let hwnd_i = hwnd.0 as isize;
        std::thread::spawn(move || {
            let segs = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::stt::transcribe_segments(&url, &wav, &lang, &hot, &prompt).map(|(_, s)| s).unwrap_or_default()
            }))
            .unwrap_or_default();
            let id = stash_partial(segs);
            unsafe {
                let _ = PostMessageW(HWND(hwnd_i as *mut core::ffi::c_void), WM_APP_STREAM_PARTIAL, WPARAM(0), LPARAM(id as isize));
            }
        });
    }

    // Приём частичного распознавания (LocalAgreement-2): зафиксировать устоявшиеся сегменты, сдвинуть окно.
    pub fn on_partial(&mut self, id: u64) {
        let segs = take_partial(id).unwrap_or_default();
        let recording = self.state == VoiceState::Recording;
        let Some(st) = self.stream.as_mut() else { return };
        st.in_flight = false;
        if !recording {
            return; // остановились/сменили состояние -> partial неактуален
        }
        let cur: Vec<String> = segs.iter().map(|s| s.text.clone()).collect();
        let n = crate::transform::committed_segs(&st.prev_texts, &cur, STREAM_MARGIN_SEGS);
        if n > 0 {
            let piece: String = segs[..n].iter().map(|s| s.text.as_str()).collect();
            st.committed_text = crate::transform::merge_committed(&st.committed_text, &piece);
            let advance = (segs[n - 1].end.max(0.0) * st.rate as f32).round() as usize;
            st.window_start += advance;
            st.prev_texts = cur[n..].to_vec();
            vlog(&format!("on_partial: +{n} сегм, committed={:?}, window_start={}", st.committed_text, st.window_start));
        } else {
            st.prev_texts = cur;
        }
    }
    // END_BLOCK_STREAM
}

#[cfg(test)]
mod tests {
    use super::*;
    use VoiceState::*;

    #[test]
    fn next_state_transitions() {
        assert_eq!(next_state(Idle, VoiceEvent::Toggle), Recording);
        assert_eq!(next_state(Recording, VoiceEvent::Toggle), Transcribing);
        assert_eq!(next_state(Transcribing, VoiceEvent::Toggle), Transcribing); // занято
        assert_eq!(next_state(Transcribing, VoiceEvent::Done), Idle);
        assert_eq!(next_state(Idle, VoiceEvent::Done), Idle);
    }

    #[test]
    fn stash_take_result_roundtrip_and_unknown_none() {
        // audit #3: id-реестр вместо сырого указателя в lparam
        let id = stash_result("привет мир".into());
        assert_eq!(take_result(id).as_deref(), Some("привет мир"));
        assert_eq!(take_result(id), None); // повторный take -> уже забрано
        assert_eq!(take_result(u64::MAX), None); // неизвестный id (мусор из чужого сообщения) -> None, без разыменования
    }
}
