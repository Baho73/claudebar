// FILE: src/voice.rs
// VERSION: 1.2.0
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
//   WM_APP_VOICE_DONE    - оконное сообщение: worker -> UI (lparam = Box<String> с распознанным текстом)
//   Voice                - держатель состояния + активного Recorder (legacy) или персистентного Mic (always-on)
//   Voice::toggle        - переключатель записи/распознавания (always-on -> arm/disarm Mic, иначе Recorder cold-start)
//   Voice::set_always_on - старт/дроп персистентного Mic по галочке (только на Idle)
//   Voice::on_done       - вернуть в Idle после доставки текста
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.2.0 - Phase-22: always-on микрофон + pre-roll. Voice держит Option<Mic>; set_always_on(on) стартует/дропает персистентный Mic (только на Idle). toggle/stop/poll/level ветвятся: при Mic -> arm/disarm_take (тёплый поток, pre-roll, первое слово не теряется), иначе legacy Recorder (cold-start). Галочка деф. ВЫКЛ (M-CONFIG voice_always_on).
//   v1.1.0 - Phase-19 доводка: авто-стоп по тишине (poll: 2с после речи / 8с без речи), короткие тоны старта/конца (Beep, в фоне), уровень микрофона (level), диагностический лог (vlog в voice.log). Подтверждено рабочим; валил поведенческий AV (Касперский), не баг.
//   v1.0.0 - Phase-19 step-1: стейт-машина голосового ввода. Recorder (!Send) живёт на UI-потоке
//                между нажатиями; распознавание (M-STT) + чистка (M-TRANSFORM) — в worker-потоке, результат
//                в UI через PostMessage (lparam = Box<String>). HWND передаём в поток как isize.
// END_CHANGE_SUMMARY

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Diagnostics::Debug::Beep;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

// Сообщение «распознавание готово»: lparam = *mut String (Box::into_raw); WM_APP+1 занят поиском.
pub const WM_APP_VOICE_DONE: u32 = WM_APP + 2;

// Авто-стоп записи по молчанию (чтобы не писать часами, если забыл выключить).
const SILENCE_STOP_SECS: f32 = 2.0; // была речь -> стоп после стольких секунд тишины
const NO_SPEECH_CAP_SECS: f32 = 8.0; // речи вообще не было -> стоп через столько

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
}

impl Default for Voice {
    fn default() -> Self {
        Voice { state: VoiceState::Idle, rec: None, mic: None }
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
    pub fn set_always_on(&mut self, on: bool) {
        if self.state != VoiceState::Idle {
            vlog("set_always_on: не на Idle — отложено (без краха)");
            return;
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
        vlog(&format!("stop ({why}): Recording -> Transcribing (wav {} байт)", wav.len()));
        self.spawn_worker(hwnd, cfg, wav);
    }
    // END_BLOCK_STOP_REC

    // START_BLOCK_POLL_SILENCE
    // Авто-стоп по тишине: вызывается из таймера, пока идёт запись. true = состояние изменилось.
    pub fn poll(&mut self, hwnd: HWND, cfg: &crate::config::Config) -> bool {
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
    }

    // START_BLOCK_SPAWN_WORKER
    // Распознать WAV в фоне (M-STT -> M-TRANSFORM) и отдать текст в UI через PostMessage.
    fn spawn_worker(&self, hwnd: HWND, cfg: &crate::config::Config, wav: Vec<u8>) {
        let url = cfg.whisper_url.clone();
        let lang = cfg.voice_language.clone();
        let hot = cfg.hotwords.clone();
        let prompt = cfg.initial_prompt.clone();
        let vocab = crate::config::parse_vocab(&cfg.vocab);
        let hwnd_i = hwnd.0 as isize; // HWND !Send -> переносим как isize
        vlog(&format!("worker: POST {url} (wav {} байт, lang={lang})", wav.len()));
        std::thread::spawn(move || {
            let text = match crate::stt::transcribe(&url, &wav, &lang, &hot, &prompt) {
                Ok(t) => {
                    let out = crate::transform::process(&t, &vocab);
                    vlog(&format!("worker: STT ok, raw={:?} -> out={:?}", t, out));
                    out
                }
                Err(e) => {
                    vlog(&format!("worker: STT FAILED: {e}"));
                    String::new()
                }
            };
            let boxed = Box::into_raw(Box::new(text)) as isize;
            unsafe {
                let _ = PostMessageW(
                    HWND(hwnd_i as *mut core::ffi::c_void),
                    WM_APP_VOICE_DONE,
                    WPARAM(0),
                    LPARAM(boxed),
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
}
