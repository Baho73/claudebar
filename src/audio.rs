// FILE: src/audio.rs
// VERSION: 1.0.0
// START_MODULE_CONTRACT
//   PURPOSE: Захват аудио с микрофона по умолчанию (cpal) в WAV 16-bit PCM mono в память для голосового ввода.
//   SCOPE: start_recording (открыть дефолтный вход, начать поток-захват в общий буфер), Recorder::stop (остановить, вернуть WAV-байты), чистая encode_wav (RIFF/fmt/data).
//   DEPENDS: cpal (захват), std::sync (общий буфер)
//   LINKS: M-AUDIO
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   start_recording - начать захват с дефолтного микрофона -> Recorder (поток + буфер)
//   Recorder        - хэндл активной записи (cpal Stream + общий буфер сэмплов + частота)
//   Recorder::stop  - остановить поток, собрать WAV-байты из накопленных сэмплов
//   encode_wav      - чистое: &[i16] + rate + channels -> WAV-байты (RIFF/fmt/data)
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.1.0 - Phase-19 доводка: Recorder += trailing_silence/had_speech/duration (авто-стоп по тишине) + level (индикатор громкости, пик ~100мс).
//   v1.0.0 - Phase-18 step-2: захват аудио (cpal) + encode_wav. Микрофон по умолчанию,
//                микширование в моно (первый канал кадра), частота устройства как есть (whisper-dictate
//                ресемплит через ffmpeg). Формат сэмплов f32/i16/u16 -> i16. Первая аудио-зависимость
//                (cpal 0.15, сборка под windows-gnu проверена спайком).
// END_CHANGE_SUMMARY

use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

// Порог амплитуды i16, выше которого сэмпл считается «звуком» (а не тишиной) — для авто-стопа.
const LOUD_TH: i32 = 700;

// START_CONTRACT: Recorder
//   PURPOSE: Хэндл активной записи. Пока жив — cpal-поток пишет сэмплы в общий буфер; stop() завершает.
//   INPUTS: { _stream: cpal::Stream, buf: общий буфер i16, rate: частота устройства }
//   OUTPUTS: { stop -> Vec<u8> WAV }
//   SIDE_EFFECTS: держит открытым аудио-поток ОС до stop/drop
//   LINKS: M-VOICE (владелец между нажатиями хоткея)
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
        let g = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        let mut n = 0usize;
        for &s in g.iter().rev() {
            if (s as i32).abs() > LOUD_TH {
                break;
            }
            n += 1;
        }
        n as f32 / self.rate.max(1) as f32
    }

    // Была ли вообще речь (хоть один сэмпл громче порога) — чтобы не резать «медленный старт».
    pub fn had_speech(&self) -> bool {
        let g = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        g.iter().any(|&s| (s as i32).abs() > LOUD_TH)
    }

    // Длительность записи, сек.
    pub fn duration(&self) -> f32 {
        let g = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        g.len() as f32 / self.rate.max(1) as f32
    }

    // Текущий уровень входного сигнала (пик последних ~100мс), 0.0..1.0 — для индикатора громкости.
    pub fn level(&self) -> f32 {
        let g = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        let window = (self.rate / 10).max(1) as usize; // ~100мс
        let start = g.len().saturating_sub(window);
        let peak = g[start..].iter().map(|&s| (s as i32).unsigned_abs()).max().unwrap_or(0);
        (peak as f32 / 32767.0).min(1.0)
    }
}

// START_CONTRACT: start_recording
//   PURPOSE: Начать захват с микрофона по умолчанию; вернуть Recorder, копящий сэмплы.
//   INPUTS: {}
//   OUTPUTS: { Result<Recorder, String> — Err("NO_INPUT_DEVICE"/"STREAM_BUILD_FAILED"/...) }
//   SIDE_EFFECTS: открывает входной аудио-поток ОС
//   LINKS: M-AUDIO
// END_CONTRACT: start_recording
pub fn start_recording() -> Result<Recorder, String> {
    // START_BLOCK_OPEN_DEVICE
    let host = cpal::default_host();
    let device = host.default_input_device().ok_or("NO_INPUT_DEVICE")?;
    let supported = device
        .default_input_config()
        .map_err(|e| format!("CONFIG_FAILED: {e}"))?;
    let rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let fmt = supported.sample_format();
    let cfg: cpal::StreamConfig = supported.into();
    // END_BLOCK_OPEN_DEVICE

    // START_BLOCK_BUILD_STREAM
    let buf = Arc::new(Mutex::new(Vec::<i16>::new()));
    let err_fn = |e: cpal::StreamError| eprintln!("[M-AUDIO][stream] {e}");
    let stream = match fmt {
        cpal::SampleFormat::F32 => {
            let b = buf.clone();
            device.build_input_stream(
                &cfg,
                move |data: &[f32], _: &_| push_mono(&b, data, channels, |s| (s.clamp(-1.0, 1.0) * 32767.0) as i16),
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let b = buf.clone();
            device.build_input_stream(&cfg, move |data: &[i16], _: &_| push_mono(&b, data, channels, |s| s), err_fn, None)
        }
        cpal::SampleFormat::U16 => {
            let b = buf.clone();
            device.build_input_stream(
                &cfg,
                move |data: &[u16], _: &_| push_mono(&b, data, channels, |s| (s as i32 - 32768) as i16),
                err_fn,
                None,
            )
        }
        other => return Err(format!("UNSUPPORTED_FORMAT: {other:?}")),
    }
    .map_err(|e| format!("STREAM_BUILD_FAILED: {e}"))?;
    stream.play().map_err(|e| format!("STREAM_PLAY_FAILED: {e}"))?;
    // END_BLOCK_BUILD_STREAM

    Ok(Recorder { _stream: stream, buf, rate })
}

// Микшировать кадры в моно (первый канал каждого кадра) и дописать в общий буфер.
fn push_mono<T: Copy>(buf: &Arc<Mutex<Vec<i16>>>, data: &[T], channels: usize, to_i16: impl Fn(T) -> i16) {
    if channels == 0 {
        return;
    }
    if let Ok(mut g) = buf.lock() {
        for frame in data.chunks(channels) {
            g.push(to_i16(frame[0]));
        }
    }
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
}
