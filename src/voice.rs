// FILE: src/voice.rs
// VERSION: 1.0.0
// START_MODULE_CONTRACT
//   PURPOSE: Оркестрация голосового ввода: стейт-машина idle->recording->transcribing, спавн worker-потока распознавания, доставка текста в UI.
//   SCOPE: VoiceState/VoiceEvent + чистая next_state; Voice (toggle: старт/стоп записи + спавн worker stt->transform; on_done; state). Worker шлёт текст в UI через PostMessage(WM_APP_VOICE_DONE).
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
//   Voice                - держатель состояния + активного Recorder
//   Voice::toggle        - переключатель записи/распознавания (side-effect: захват, спавн потока)
//   Voice::on_done       - вернуть в Idle после доставки текста
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.0.0 - Phase-19 step-1: стейт-машина голосового ввода. Recorder (!Send) живёт на UI-потоке
//                между нажатиями; распознавание (M-STT) + чистка (M-TRANSFORM) — в worker-потоке, результат
//                в UI через PostMessage (lparam = Box<String>). HWND передаём в поток как isize.
// END_CHANGE_SUMMARY

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

// Сообщение «распознавание готово»: lparam = *mut String (Box::into_raw); WM_APP+1 занят поиском.
pub const WM_APP_VOICE_DONE: u32 = WM_APP + 2;

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
    rec: Option<crate::audio::Recorder>,
}

impl Default for Voice {
    fn default() -> Self {
        Voice { state: VoiceState::Idle, rec: None }
    }
}

impl Voice {
    pub fn new() -> Self {
        Voice::default()
    }

    pub fn state(&self) -> VoiceState {
        self.state
    }

    // Переключатель по хоткею: старт записи / стоп+распознавание / игнор (занято).
    // Состояние меняем через next_state (единый источник переходов), но только при успехе side-effect.
    pub fn toggle(&mut self, hwnd: HWND, cfg: &crate::config::Config) {
        match self.state {
            VoiceState::Idle => {
                // START_BLOCK_START_REC
                match crate::audio::start_recording() {
                    Ok(r) => {
                        self.rec = Some(r);
                        self.state = next_state(self.state, VoiceEvent::Toggle); // -> Recording
                    }
                    Err(e) => eprintln!("[M-VOICE][toggle][START_REC] {e}"),
                }
                // END_BLOCK_START_REC
            }
            VoiceState::Recording => {
                // START_BLOCK_STOP_REC
                let Some(rec) = self.rec.take() else {
                    self.state = VoiceState::Idle;
                    return;
                };
                let wav = rec.stop();
                self.state = next_state(self.state, VoiceEvent::Toggle); // -> Transcribing
                self.spawn_worker(hwnd, cfg, wav);
                // END_BLOCK_STOP_REC
            }
            VoiceState::Transcribing => { /* занято — игнор */ }
        }
    }

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
        std::thread::spawn(move || {
            let text = match crate::stt::transcribe(&url, &wav, &lang, &hot, &prompt) {
                Ok(t) => crate::transform::process(&t, &vocab),
                Err(e) => {
                    eprintln!("[M-VOICE][worker][STT] {e}");
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
