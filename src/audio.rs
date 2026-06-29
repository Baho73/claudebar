// FILE: src/audio.rs
// VERSION: 1.2.0
// START_MODULE_CONTRACT
//   PURPOSE: Захват аудио с микрофона по умолчанию (cpal) в WAV 16-bit PCM mono в память для голосового ввода.
//   SCOPE: start_recording (разовая запись -> Recorder), start_persistent (always-on поток + кольцо pre-roll -> Mic, arm/disarm_take), чистая encode_wav, чистые ring_keep/pre_roll_samples (логика кольца).
//   DEPENDS: cpal (захват), std::sync (общий буфер)
//   LINKS: M-AUDIO
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   start_recording  - разовый захват с дефолтного микрофона -> Recorder (поток создаётся/дропается на запись)
//   start_persistent - always-on захват -> Mic (поток крутится всегда, !armed -> кольцо pre-roll, armed -> запись)
//   Recorder         - хэндл разовой записи (cpal Stream + общий буфер сэмплов + частота)
//   Recorder::stop   - остановить поток, собрать WAV-байты
//   Mic              - персистентный хэндл: arm (начать копить), disarm_take (WAV pre-roll+live, сброс в кольцо)
//   ring_keep        - чистое: сколько сэмплов выкинуть с переда, чтобы кольцо держало <= cap
//   pre_roll_samples - чистое: rate*ms/1000 -> число сэмплов pre-roll
//   encode_wav       - чистое: &[i16] + rate + channels -> WAV-байты (RIFF/fmt/data)
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.2.0 - Phase-22: always-on микрофон + pre-roll. start_persistent -> Mic: поток крутится постоянно,
//                при !armed обрезает перёд до cap (кольцо ~500мс), arm() перестаёт обрезать (кольцо = голова записи),
//                disarm_take() -> WAV(pre-roll+live), сброс в кольцо без дропа потока. Чистые ring_keep/pre_roll_samples.
//                Общие helpers open_default_input/build_mono_stream/*_of (убрано дублирование с Recorder, push_mono удалён).
//   v1.1.0 - Phase-19 доводка: Recorder += trailing_silence/had_speech/duration (авто-стоп по тишине) + level (индикатор громкости, пик ~100мс).
//   v1.0.0 - Phase-18 step-2: захват аудио (cpal) + encode_wav. Микрофон по умолчанию,
//                микширование в моно (первый канал кадра), частота устройства как есть (whisper-dictate
//                ресемплит через ffmpeg). Формат сэмплов f32/i16/u16 -> i16. Первая аудио-зависимость
//                (cpal 0.15, сборка под windows-gnu проверена спайком).
// END_CHANGE_SUMMARY

use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

// Порог амплитуды i16, выше которого сэмпл считается «звуком» (а не тишиной) — для авто-стопа.
const LOUD_TH: i32 = 700;

// Длина pre-roll кольца (мс): сколько аудио до нажатия хоткея становится «головой» записи при always-on.
pub const PRE_ROLL_MS: u32 = 500;

// START_CONTRACT: ring_keep
//   PURPOSE: Чистое — сколько сэмплов выкинуть с переда буфера, чтобы кольцо держало не более cap.
//   INPUTS: { len: usize (текущая длина), cap: usize (потолок кольца) }
//   OUTPUTS: { usize — drop_front (0 если len <= cap) }
//   SIDE_EFFECTS: none
//   LINKS: M-AUDIO
// END_CONTRACT: ring_keep
fn ring_keep(len: usize, cap: usize) -> usize {
    len.saturating_sub(cap)
}

// START_CONTRACT: pre_roll_samples
//   PURPOSE: Чистое — число сэмплов pre-roll для частоты и длительности.
//   INPUTS: { rate: u32 (Гц), ms: u32 (длительность) }
//   OUTPUTS: { usize — rate*ms/1000 }
//   SIDE_EFFECTS: none
//   LINKS: M-AUDIO
// END_CONTRACT: pre_roll_samples
fn pre_roll_samples(rate: u32, ms: u32) -> usize {
    (rate as u64 * ms as u64 / 1000) as usize
}

// Чистые вычисления по срезу сэмплов — общие для Recorder и Mic (без дублирования).
fn had_speech_of(s: &[i16]) -> bool {
    s.iter().any(|&x| (x as i32).abs() > LOUD_TH)
}
fn trailing_silence_of(s: &[i16], rate: u32) -> f32 {
    let mut n = 0usize;
    for &x in s.iter().rev() {
        if (x as i32).abs() > LOUD_TH {
            break;
        }
        n += 1;
    }
    n as f32 / rate.max(1) as f32
}
fn duration_of(s: &[i16], rate: u32) -> f32 {
    s.len() as f32 / rate.max(1) as f32
}
fn level_of(s: &[i16], rate: u32) -> f32 {
    let window = (rate / 10).max(1) as usize; // ~100мс
    let start = s.len().saturating_sub(window);
    let peak = s[start..].iter().map(|&x| (x as i32).unsigned_abs()).max().unwrap_or(0);
    (peak as f32 / 32767.0).min(1.0)
}

// START_CONTRACT: Recorder
//   PURPOSE: Хэндл разовой записи. Пока жив — cpal-поток пишет сэмплы в общий буфер; stop() завершает.
//   INPUTS: { _stream: cpal::Stream, buf: общий буфер i16, rate: частота устройства }
//   OUTPUTS: { stop -> Vec<u8> WAV }
//   SIDE_EFFECTS: держит открытым аудио-поток ОС до stop/drop
//   LINKS: M-VOICE (владелец между нажатиями хоткея при выключенном always-on)
// END_CONTRACT: Recorder
pub struct Recorder {
    _stream: cpal::Stream,
    buf: Arc<Mutex<Vec<i16>>>,
    rate: u32,
}

impl Recorder {
    // Остановить захват (drop потока) и собрать накопленное в WAV (моно, частота устройства).
    pub fn stop(self) -> Vec<u8> {
        drop(self._stream); // остановка захвата
        let samples = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        encode_wav(&samples, self.rate, 1)
    }

    // Секунд тишины в хвосте записи (для авто-стопа по молчанию).
    pub fn trailing_silence(&self) -> f32 {
        trailing_silence_of(&self.buf.lock().unwrap_or_else(|e| e.into_inner()), self.rate)
    }

    // Была ли вообще речь (хоть один сэмпл громче порога) — чтобы не резать «медленный старт».
    pub fn had_speech(&self) -> bool {
        had_speech_of(&self.buf.lock().unwrap_or_else(|e| e.into_inner()))
    }

    // Длительность записи, сек.
    pub fn duration(&self) -> f32 {
        duration_of(&self.buf.lock().unwrap_or_else(|e| e.into_inner()), self.rate)
    }

    // Текущий уровень входного сигнала (пик последних ~100мс), 0.0..1.0 — для индикатора громкости.
    pub fn level(&self) -> f32 {
        level_of(&self.buf.lock().unwrap_or_else(|e| e.into_inner()), self.rate)
    }
}

// START_CONTRACT: start_recording
//   PURPOSE: Начать разовый захват с микрофона по умолчанию; вернуть Recorder, копящий сэмплы.
//   INPUTS: {}
//   OUTPUTS: { Result<Recorder, String> — Err("NO_INPUT_DEVICE"/"STREAM_BUILD_FAILED"/...) }
//   SIDE_EFFECTS: открывает входной аудио-поток ОС
//   LINKS: M-AUDIO
// END_CONTRACT: start_recording
pub fn start_recording() -> Result<Recorder, String> {
    let (device, cfg, fmt, rate, channels) = open_default_input()?;
    let buf = Arc::new(Mutex::new(Vec::<i16>::new()));
    let b = buf.clone();
    let stream = build_mono_stream(&device, &cfg, fmt, channels, move |mono| {
        if let Ok(mut g) = b.lock() {
            g.extend_from_slice(mono);
        }
    })?;
    stream.play().map_err(|e| format!("STREAM_PLAY_FAILED: {e}"))?;
    Ok(Recorder { _stream: stream, buf, rate })
}

// START_CONTRACT: Mic
//   PURPOSE: Персистентный always-on захват. Поток крутится всегда: при !armed буфер обрезается до cap
//            (кольцо pre-roll), при armed копит -> в буфере уже лежит pre-roll как «голова» записи.
//   INPUTS: { _stream: cpal::Stream, buf: Arc<Mutex<Ring>>, rate }
//   OUTPUTS: { arm() -> копим; disarm_take() -> Vec<u8> WAV(pre-roll+live), сброс в кольцо }
//   SIDE_EFFECTS: держит аудио-поток ОС постоянно (индикатор микрофона горит), пока Mic жив
//   LINKS: M-VOICE (владелец при включённом always-on)
// END_CONTRACT: Mic
struct Ring {
    samples: Vec<i16>,
    armed: bool,
}

pub struct Mic {
    _stream: cpal::Stream,
    buf: Arc<Mutex<Ring>>,
    rate: u32,
}

impl Mic {
    // Начать запись: перестать обрезать кольцо. В буфере уже последние ~PRE_ROLL_MS = голова записи.
    pub fn arm(&self) {
        if let Ok(mut g) = self.buf.lock() {
            g.armed = true;
        }
    }

    // Завершить запись: собрать WAV из накопленного (pre-roll+live), сбросить буфер в кольцо. Поток жив.
    pub fn disarm_take(&self) -> Vec<u8> {
        let mut g = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        let wav = encode_wav(&g.samples, self.rate, 1);
        g.samples.clear();
        g.armed = false;
        wav
    }

    pub fn trailing_silence(&self) -> f32 {
        trailing_silence_of(&self.buf.lock().unwrap_or_else(|e| e.into_inner()).samples, self.rate)
    }
    pub fn had_speech(&self) -> bool {
        had_speech_of(&self.buf.lock().unwrap_or_else(|e| e.into_inner()).samples)
    }
    pub fn duration(&self) -> f32 {
        duration_of(&self.buf.lock().unwrap_or_else(|e| e.into_inner()).samples, self.rate)
    }
    pub fn level(&self) -> f32 {
        level_of(&self.buf.lock().unwrap_or_else(|e| e.into_inner()).samples, self.rate)
    }
}

// START_CONTRACT: start_persistent
//   PURPOSE: Запустить always-on поток захвата в кольцевой буфер pre-roll; вернуть Mic.
//   INPUTS: {}
//   OUTPUTS: { Result<Mic, String> — те же Err, что start_recording }
//   SIDE_EFFECTS: открывает входной аудио-поток ОС и держит его постоянно (индикатор микрофона горит)
//   LINKS: M-AUDIO
// END_CONTRACT: start_persistent
pub fn start_persistent() -> Result<Mic, String> {
    let (device, cfg, fmt, rate, channels) = open_default_input()?;
    let cap = pre_roll_samples(rate, PRE_ROLL_MS);
    let buf = Arc::new(Mutex::new(Ring { samples: Vec::new(), armed: false }));
    let b = buf.clone();
    let stream = build_mono_stream(&device, &cfg, fmt, channels, move |mono| {
        if let Ok(mut g) = b.lock() {
            g.samples.extend_from_slice(mono);
            if !g.armed {
                let drop = ring_keep(g.samples.len(), cap);
                if drop > 0 {
                    g.samples.drain(0..drop); // ponytail: O(n) drain переда; VecDeque если cap сильно вырастет
                }
            }
        }
    })?;
    stream.play().map_err(|e| format!("STREAM_PLAY_FAILED: {e}"))?;
    Ok(Mic { _stream: stream, buf, rate })
}

// Открыть микрофон по умолчанию -> (устройство, конфиг потока, формат сэмплов, частота, число каналов).
fn open_default_input() -> Result<(cpal::Device, cpal::StreamConfig, cpal::SampleFormat, u32, usize), String> {
    let host = cpal::default_host();
    let device = host.default_input_device().ok_or("NO_INPUT_DEVICE")?;
    let supported = device
        .default_input_config()
        .map_err(|e| format!("CONFIG_FAILED: {e}"))?;
    let rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let fmt = supported.sample_format();
    let cfg: cpal::StreamConfig = supported.into();
    Ok((device, cfg, fmt, rate, channels))
}

// Построить входной поток, микширующий кадры в моно (первый канал) и отдающий срез i16 в sink.
fn build_mono_stream(
    device: &cpal::Device,
    cfg: &cpal::StreamConfig,
    fmt: cpal::SampleFormat,
    channels: usize,
    sink: impl Fn(&[i16]) + Send + Clone + 'static,
) -> Result<cpal::Stream, String> {
    let err_fn = |e: cpal::StreamError| eprintln!("[M-AUDIO][stream] {e}");
    let stream = match fmt {
        cpal::SampleFormat::F32 => {
            let s = sink.clone();
            device.build_input_stream(
                cfg,
                move |data: &[f32], _: &_| {
                    if channels == 0 {
                        return;
                    }
                    let mono: Vec<i16> = data.chunks(channels).map(|f| (f[0].clamp(-1.0, 1.0) * 32767.0) as i16).collect();
                    s(&mono);
                },
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let s = sink.clone();
            device.build_input_stream(
                cfg,
                move |data: &[i16], _: &_| {
                    if channels == 0 {
                        return;
                    }
                    let mono: Vec<i16> = data.chunks(channels).map(|f| f[0]).collect();
                    s(&mono);
                },
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let s = sink.clone();
            device.build_input_stream(
                cfg,
                move |data: &[u16], _: &_| {
                    if channels == 0 {
                        return;
                    }
                    let mono: Vec<i16> = data.chunks(channels).map(|f| (f[0] as i32 - 32768) as i16).collect();
                    s(&mono);
                },
                err_fn,
                None,
            )
        }
        other => return Err(format!("UNSUPPORTED_FORMAT: {other:?}")),
    }
    .map_err(|e| format!("STREAM_BUILD_FAILED: {e}"))?;
    Ok(stream)
}

// START_CONTRACT: encode_wav
//   PURPOSE: Обернуть PCM-сэмплы i16 в минимальный WAV (RIFF/fmt/data, без доп. чанков).
//   INPUTS: { samples: &[i16], rate: u32 (Гц), channels: u16 }
//   OUTPUTS: { Vec<u8> — валидный WAV 16-bit PCM }
//   SIDE_EFFECTS: none
//   LINKS: M-AUDIO, M-STT (отправляется в /transcribe)
// END_CONTRACT: encode_wav
pub fn encode_wav(samples: &[i16], rate: u32, channels: u16) -> Vec<u8> {
    let bits: u16 = 16;
    let block_align: u16 = channels * bits / 8;
    let byte_rate: u32 = rate * block_align as u32;
    let data_len: u32 = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // размер fmt-чанка
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16le(b: &[u8], o: usize) -> u16 {
        u16::from_le_bytes([b[o], b[o + 1]])
    }
    fn u32le(b: &[u8], o: usize) -> u32 {
        u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
    }

    #[test]
    fn encode_wav_header_and_data() {
        let samples = [0i16, 1000, -1000, 32767];
        let w = encode_wav(&samples, 16000, 1);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(u32le(&w, 4), 36 + 2 * 4); // RIFF size = 36 + data
        assert_eq!(&w[8..12], b"WAVE");
        assert_eq!(&w[12..16], b"fmt ");
        assert_eq!(u32le(&w, 16), 16); // fmt size
        assert_eq!(u16le(&w, 20), 1); // PCM
        assert_eq!(u16le(&w, 22), 1); // channels
        assert_eq!(u32le(&w, 24), 16000); // rate
        assert_eq!(u32le(&w, 28), 16000 * 2); // byte rate (mono 16-bit)
        assert_eq!(u16le(&w, 32), 2); // block align
        assert_eq!(u16le(&w, 34), 16); // bits
        assert_eq!(&w[36..40], b"data");
        assert_eq!(u32le(&w, 40), 2 * 4); // data len
        assert_eq!(w.len(), 44 + 2 * 4);
        // первые два сэмпла little-endian
        assert_eq!(i16::from_le_bytes([w[44], w[45]]), 0);
        assert_eq!(i16::from_le_bytes([w[46], w[47]]), 1000);
    }

    #[test]
    fn encode_wav_empty() {
        let w = encode_wav(&[], 48000, 1);
        assert_eq!(w.len(), 44); // только заголовок
        assert_eq!(u32le(&w, 40), 0); // нет данных
        assert_eq!(u32le(&w, 24), 48000);
    }

    #[test]
    fn ring_keep_basic() {
        assert_eq!(ring_keep(0, 100), 0); // пусто -> ничего не выкидываем
        assert_eq!(ring_keep(100, 100), 0); // ровно cap -> 0
        assert_eq!(ring_keep(50, 100), 0); // меньше cap -> 0
        assert_eq!(ring_keep(150, 100), 50); // больше cap -> выкинуть лишнее с переда
    }

    #[test]
    fn pre_roll_samples_basic() {
        assert_eq!(pre_roll_samples(48000, 500), 24000);
        assert_eq!(pre_roll_samples(16000, 500), 8000);
        assert_eq!(pre_roll_samples(44100, 300), 13230);
        assert_eq!(pre_roll_samples(48000, 0), 0);
    }
}
