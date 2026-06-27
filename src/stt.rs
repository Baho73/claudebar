// FILE: src/stt.rs
// VERSION: 1.0.0
// START_MODULE_CONTRACT
//   PURPOSE: Синхронное распознавание речи: POST WAV на whisper-dictate /transcribe (localhost http через std::net), разбор {"text"}.
//   SCOPE: transcribe (TcpStream POST multipart, чтение ответа); чистые build_multipart / parse_text / parse_url.
//   DEPENDS: none (URL/язык/словарь приходят параметрами из M-CONFIG; JSON через serde_json)
//   LINKS: M-STT
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   transcribe       - POST WAV на /transcribe (TcpStream) -> распознанный текст или Err
//   build_multipart  - чистое: текстовые поля + файл -> тело multipart/form-data (байты)
//   parse_text       - чистое: JSON ответа -> Option<text> (serde_json)
//   parse_url        - чистое: http://host:port/path -> (host, port, path)
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.0.0 - Phase-18 step-3: клиент whisper-dictate. Синхронный POST multipart WAV через
//                std::net::TcpStream (без HTTP-крейта, как M-SDAEMON); чистые build_multipart/parse_text/parse_url.
//                Опц. поля hotwords/initial_prompt шлются только если непусты (модель по умолчанию их не держит).
// END_CHANGE_SUMMARY

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

// START_CONTRACT: parse_url
//   PURPOSE: Разобрать http-URL на (host, port, path) для TcpStream.
//   INPUTS: { url: &str - "http://host[:port][/path]" }
//   OUTPUTS: { Option<(String host, u16 port, String path)> - порт по умолчанию 80, path по умолчанию "/" }
//   SIDE_EFFECTS: none
// END_CONTRACT: parse_url
pub fn parse_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("http://").unwrap_or(url);
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().ok()?),
        None => (authority.to_string(), 80u16),
    };
    if host.is_empty() {
        return None;
    }
    Some((host, port, path))
}

// START_CONTRACT: build_multipart
//   PURPOSE: Собрать тело multipart/form-data: текстовые поля + один файл.
//   INPUTS: { boundary: &str; fields: &[(name, value)]; filename: &str; file: &[u8] }
//   OUTPUTS: { Vec<u8> - тело (бинарно-безопасно) }
//   SIDE_EFFECTS: none
// END_CONTRACT: build_multipart
pub fn build_multipart(boundary: &str, fields: &[(&str, &str)], filename: &str, file: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(file.len() + 256);
    for (name, value) in fields {
        out.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes(),
        );
    }
    out.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: audio/wav\r\n\r\n").as_bytes(),
    );
    out.extend_from_slice(file);
    out.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    out
}

// START_CONTRACT: parse_text
//   PURPOSE: Извлечь распознанный текст из JSON ответа {"text": "..."}.
//   INPUTS: { body: &str - тело ответа (возможно с обрамляющим мусором) }
//   OUTPUTS: { Option<String> - текст; None при отсутствии поля/битом JSON }
//   SIDE_EFFECTS: none
// END_CONTRACT: parse_text
pub fn parse_text(body: &str) -> Option<String> {
    let start = body.find('{')?;
    let end = body.rfind('}')?;
    if end < start {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&body[start..=end]).ok()?;
    v.get("text")?.as_str().map(|s| s.to_string())
}

// START_CONTRACT: transcribe
//   PURPOSE: Распознать WAV через whisper-dictate /transcribe (синхронно).
//   INPUTS: { url: &str; wav: &[u8]; language: &str; hotwords: &str; initial_prompt: &str }
//   OUTPUTS: { Result<String, String> - текст или код ошибки (CONNECT_FAILED/HTTP_ERROR/PARSE_FAILED/...) }
//   SIDE_EFFECTS: TCP-соединение на localhost (POST), чтение ответа до закрытия
//   LINKS: M-STT, M-VOICE (вызывается в worker-потоке)
// END_CONTRACT: transcribe
pub fn transcribe(url: &str, wav: &[u8], language: &str, hotwords: &str, initial_prompt: &str) -> Result<String, String> {
    // START_BLOCK_CONNECT
    let (host, port, path) = parse_url(url).ok_or("BAD_URL")?;
    let addr = format!("{host}:{port}");
    let sockaddr = addr
        .to_socket_addrs()
        .map_err(|e| format!("RESOLVE_FAILED: {e}"))?
        .next()
        .ok_or("RESOLVE_EMPTY")?;
    let mut stream =
        TcpStream::connect_timeout(&sockaddr, Duration::from_secs(3)).map_err(|e| format!("CONNECT_FAILED: {e}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(120))); // распознавание идёт секунды
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    // END_BLOCK_CONNECT

    // START_BLOCK_SEND
    let boundary = "----claudebarvoice";
    let mut fields: Vec<(&str, &str)> = vec![("language", language)];
    if !hotwords.is_empty() {
        fields.push(("hotwords", hotwords));
    }
    if !initial_prompt.is_empty() {
        fields.push(("initial_prompt", initial_prompt));
    }
    let body = build_multipart(boundary, &fields, "audio.wav", wav);
    let header = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: multipart/form-data; boundary={boundary}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n",
        len = body.len(),
    );
    stream.write_all(header.as_bytes()).map_err(|e| format!("WRITE_FAILED: {e}"))?;
    stream.write_all(&body).map_err(|e| format!("WRITE_BODY_FAILED: {e}"))?;
    // END_BLOCK_SEND

    // START_BLOCK_READ
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).map_err(|e| format!("READ_FAILED: {e}"))?;
    let text = String::from_utf8_lossy(&buf);
    let status_ok = text.lines().next().map(|l| l.contains(" 200")).unwrap_or(false);
    let idx = text.find("\r\n\r\n").ok_or("NO_BODY")?;
    let resp_body = &text[idx + 4..];
    if !status_ok {
        return Err(format!("HTTP_ERROR: {}", text.lines().next().unwrap_or("")));
    }
    parse_text(resp_body).ok_or_else(|| "PARSE_FAILED".to_string())
    // END_BLOCK_READ
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_url_cases() {
        assert_eq!(
            parse_url("http://127.0.0.1:8771/transcribe"),
            Some(("127.0.0.1".to_string(), 8771, "/transcribe".to_string()))
        );
        assert_eq!(parse_url("http://localhost/x"), Some(("localhost".to_string(), 80, "/x".to_string())));
        assert_eq!(parse_url("http://h:9000"), Some(("h".to_string(), 9000, "/".to_string())));
        assert_eq!(parse_url("http://:8771/x"), None); // нет host
    }

    #[test]
    fn parse_text_cases() {
        assert_eq!(parse_text("{\"text\":\"привет\",\"duration\":1.9}").as_deref(), Some("привет"));
        // обрамляющий мусор (chunked-маркеры) допускается
        assert_eq!(parse_text("2a\r\n{\"text\":\"да\"}\r\n0\r\n").as_deref(), Some("да"));
        assert_eq!(parse_text("{}"), None);
        assert_eq!(parse_text(""), None);
        assert_eq!(parse_text("не json"), None);
    }

    #[test]
    fn build_multipart_structure() {
        let b = build_multipart("BND", &[("language", "ru"), ("hotwords", "ClaudeBar")], "a.wav", &[1u8, 2, 3]);
        let s = String::from_utf8_lossy(&b);
        assert!(s.contains("--BND\r\nContent-Disposition: form-data; name=\"language\"\r\n\r\nru\r\n"));
        assert!(s.contains("name=\"hotwords\"\r\n\r\nClaudeBar\r\n"));
        assert!(s.contains("name=\"file\"; filename=\"a.wav\"\r\nContent-Type: audio/wav\r\n\r\n"));
        // байты файла присутствуют и тело закрыто финальным boundary
        assert!(b.windows(3).any(|w| w == [1u8, 2, 3]));
        assert!(s.ends_with("--BND--\r\n"));
    }
}
