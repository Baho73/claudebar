// FILE: src/voice.rs
// VERSION: 1.3.0
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
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.3.0 - fix(grace-fix, FPF D-13/D-14): (D-13) воркер оборачивает stt/transform в catch_unwind — паника (panic=unwind) не оставляет UI в Transcribing, текст постится всегда; watchdog в poll (transcribe_start + TRANSCRIBE_TIMEOUT_SECS=150) возвращает зависшую Transcribing в Idle. (D-14) set_always_on -> bool: тоггл «микрофон всегда вкл» во время записи не применяется и НЕ меняет конфиг (ini/галочка не разъезжаются с реальностью).
//   v1.2.1 - fix(grace-fix, audit #3): worker->UI отдаёт текст через id-реестр (stash_result/take_result), а не Box::into_raw в lparam. Раньше обработчик WM_APP_VOICE_DONE безусловно разыменовывал lparam как *mut String -> любой процесс мог послать мусор -> UB/порча кучи. Теперь мусорный id -> None. Тест stash_take_result_roundtrip_and_unknown_none.
//   v1.2.0 - Phase-22: always-on микрофон + pre-roll. Voice держит Option<Mic>; set_always_on(on) стартует/дропает персистентный Mic (только на Idle). toggle/stop/poll/level ветвятся: при Mic -> arm/disarm_take (тёплый поток, pre-roll, первое слово не теряется), иначе legacy Recorder (cold-start). Галочка деф. ВЫКЛ (M-CONFIG voice_always_on).
//   v1.1.0 - Phase-19 доводка: авто-стоп по тишине (poll: 2с после речи / 8с без речи), короткие тоны старта/конца (Beep, в фоне), уровень микрофона (level), диагностический лог (vlog в voice.log). Подтверждено рабочим; валил поведенческий AV (Касперский), не баг.
//   v1.0.0 - Phase-19 step-1: стейт-машина голосового ввода. Recorder (!Send) живёт на UI-потоке
//                между нажатиями; распознавание (M-STT) + чистка (M-TRANSFORM) — в worker-потоке, результат
//                в UI через PostMessage (lparam = Box<String>). HWND передаём в поток как isize.
// END_CHANGE_SUMMARY

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
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

// Авто-стоп записи по молчанию (чтобы не писать часами, если забыл выключить).
const SILENCE_STOP_SECS: f32 = 2.0; // была речь -> стоп после стольких секунд тишины
const NO_SPEECH_CAP_SECS: f32 = 8.0; // речи вообще не было -> стоп через столько
const TRANSCRIBE_TIMEOUT_SECS: f32 = 150.0; // watchdog: зависшая Transcribing (паника воркера/отказ Post) -> Idle (D-13)

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
pub struct Voice {
    state: VoiceState,
    rec: Option<crate::audio::Recorder>, // разовый захват (always-on ВЫКЛ): поток создаётся/дропается на запись
    mic: Option<crate::audio::Mic>, // персистентный always-on захват (always-on ВКЛ) — Phase-22
    transcribe_start: Option<Instant>, // когда вошли в Transcribing — watchdog зависшей транскрибации (D-13)
}

impl Default for Voice {
    fn default() -> Self {
        Voice { state: VoiceState::Idle, rec: None, mic: None, transcribe_start: None }
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

    // START_BLOCK_STOP_REC
    // Остановить запись и запустить распознавание (по хоткею или авто-стопу по тишине).
    fn stop_to_transcribe(&mut self, hwnd: HWND, cfg: &crate::config::Config, why: &str) {
        // always-on: забрать WAV (pre-roll+live) у Mic, поток остаётся жив; иначе остановить разовый Recorder.
        let wav = if let Some(mic) = &self.mic {
            mic.disarm_take()
        } else if let Some(rec) = self.rec.take() {
            rec.stop()
        } else {
            self.state = VoiceState::Idle;
            vlog("stop: нет источника записи -> Idle");
            return;
        };
        self.state = next_state(self.state, VoiceEvent::Toggle); // -> Transcribing
        self.transcribe_start = Some(Instant::now()); // watchdog отсчёт (D-13)
        vlog(&format!("stop ({why}): Recording -> Transcribing (wav {} байт)", wav.len()));
        self.spawn_worker(hwnd, cfg, wav);
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
    }

    // START_BLOCK_SPAWN_WORKER
    // Распознать WAV в фоне (M-STT -> M-TRANSFORM) и отдать текст в UI через PostMessage.
    fn spawn_worker(&self, hwnd: HWND, cfg: &crate::config::Config, wav: Vec<u8>) {
        let url = cfg.whisper_url.clone();
        let lang = cfg.voice_language.clone();
        let hot = cfg.hotwords.clone();
        let prompt = cfg.initial_prompt.clone();
        let vocab = crate::config::parse_vocab(&cfg.vocab);
        let trail = cfg.voice_trailing_space; // хвостовой пробел после фразы — Phase-23
        let hwnd_i = hwnd.0 as isize; // HWND !Send -> переносим как isize
        vlog(&format!("worker: POST {url} (wav {} байт, lang={lang})", wav.len()));
        std::thread::spawn(move || {
            // D-13: паника stt/transform (panic=unwind) не должна молча убить воркер —
            // ловим, отдаём пустой текст, но ВСЕГДА постим (иначе UI застрянет в Transcribing).
            let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                match crate::stt::transcribe(&url, &wav, &lang, &hot, &prompt) {
                    Ok(t) => {
                        let out = crate::transform::process(&t, &vocab, trail);
                        vlog(&format!("worker: STT ok, raw={:?} -> out={:?}", t, out));
                        out
                    }
                    Err(e) => {
                        vlog(&format!("worker: STT FAILED: {e}"));
                        String::new()
                    }
                }
            }))
            .unwrap_or_else(|_| {
                vlog("worker: PANIC перехвачена -> пустой текст (D-13)");
                String::new()
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
