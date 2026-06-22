// FILE: src/sdaemon.rs
// VERSION: 1.0.0
// START_MODULE_CONTRACT
//   PURPOSE: Клиент Python-демона clfind (dense-поиск): спавн `clfind serve` при молчащем /health, поллинг до доступности, dense-запрос по HTTP через std::net (без HTTP-крейта).
//   SCOPE: Model (статус модели); parse_health/build_dense_request (чистые, тестируемые); health/dense_search/ensure_running (интеграция: TcpStream + спавн процесса).
//   DEPENDS: none (команда запуска и порт приходят параметрами из M-CONFIG)
//   LINKS: M-SDAEMON
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   Model               - статус модели демона: Cold | Loading | Ready
//   parse_health        - чистое: тело /health -> Model
//   build_dense_request - чистое: запрос -> HTTP-текст POST /search {mode:dense}
//   health              - GET /health (TcpStream) -> Option<Model> (None = демон недоступен)
//   dense_search        - POST /search {mode:dense} -> Option<String> (тело JSON)
//   ensure_running      - доступен ли демон; иначе спавн search_cmd и поллинг /health до таймаута
//   json_quote / json_str_value - приватные помощники строк JSON
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.0.0 - Phase-12 Step 2: новый модуль клиента dense-демона (parse_health, build_dense_request чистые; health/dense_search/ensure_running на std::net).
// END_CHANGE_SUMMARY

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Model {
    Cold,
    Loading,
    Ready,
}

// Значение строкового JSON-поля "key":"value" (наивный разбор, как в M-RECENT).
fn json_str_value(body: &str, key: &str) -> Option<String> {
    let k = format!("\"{}\"", key);
    let after = &body[body.find(&k)? + k.len()..];
    let start = after.find('"')? + 1;
    let rest = &after[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

// Экранировать строку как JSON-литерал (с обрамляющими кавычками).
fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// START_CONTRACT: parse_health
//   PURPOSE: Извлечь статус модели из тела ответа /health.
//   INPUTS: { body: &str - JSON {status, model} }
//   OUTPUTS: { Model - Ready|Loading|Cold (неизвестное -> Cold) }
//   SIDE_EFFECTS: none
// END_CONTRACT: parse_health
pub fn parse_health(body: &str) -> Model {
    match json_str_value(body, "model").as_deref() {
        Some("ready") => Model::Ready,
        Some("loading") => Model::Loading,
        _ => Model::Cold,
    }
}

// START_CONTRACT: build_dense_request
//   PURPOSE: Собрать HTTP-запрос POST /search {mode:dense} для демона.
//   INPUTS: { host: &str - "127.0.0.1:port"; q: &str; scope: &str; limit: u32 }
//   OUTPUTS: { String - полный HTTP/1.1 запрос (заголовки + тело) }
//   SIDE_EFFECTS: none
// END_CONTRACT: build_dense_request
pub fn build_dense_request(host: &str, q: &str, scope: &str, limit: u32) -> String {
    let body = format!(
        "{{\"q\":{},\"scope\":\"{}\",\"mode\":\"dense\",\"limit\":{}}}",
        json_quote(q),
        scope,
        limit
    );
    format!(
        "POST /search HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         Content-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        host = host,
        len = body.len(),
        body = body
    )
}

// Прочитать ответ до закрытия соединения, вернуть тело (после \r\n\r\n).
fn read_http_body(mut stream: TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let idx = text.find("\r\n\r\n")?;
    Some(text[idx + 4..].to_string())
}

fn connect(host: &str) -> Option<TcpStream> {
    let addr = host.parse().ok()?;
    let s = TcpStream::connect_timeout(&addr, Duration::from_millis(300)).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
    Some(s)
}

// START_CONTRACT: health
//   PURPOSE: Опросить /health демона.
//   INPUTS: { port: u16 }
//   OUTPUTS: { Option<Model> - None, если демон недоступен }
//   SIDE_EFFECTS: TCP-соединение на localhost
// END_CONTRACT: health
pub fn health(port: u16) -> Option<Model> {
    let host = format!("127.0.0.1:{port}");
    let mut s = connect(&host)?;
    let req = format!("GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).ok()?;
    Some(parse_health(&read_http_body(s)?))
}

// START_CONTRACT: dense_search
//   PURPOSE: Выполнить dense-поиск на демоне.
//   INPUTS: { port: u16; q: &str; scope: &str; limit: u32 }
//   OUTPUTS: { Option<String> - сырое тело JSON ответа или None }
//   SIDE_EFFECTS: TCP-соединение на localhost
// END_CONTRACT: dense_search
pub fn dense_search(port: u16, q: &str, scope: &str, limit: u32) -> Option<String> {
    let host = format!("127.0.0.1:{port}");
    let mut s = connect(&host)?;
    s.write_all(build_dense_request(&host, q, scope, limit).as_bytes())
        .ok()?;
    read_http_body(s)
}

// START_CONTRACT: ensure_running
//   PURPOSE: Гарантировать запущенный демон: если /health недоступен — спавн search_cmd и поллинг до таймаута.
//   INPUTS: { search_cmd: &str; port: u16; timeout: Duration }
//   OUTPUTS: { bool - true, если демон доступен по /health }
//   SIDE_EFFECTS: может запустить внешний процесс (Command), TCP-опрос
// END_CONTRACT: ensure_running
pub fn ensure_running(search_cmd: &str, port: u16, timeout: Duration) -> bool {
    if health(port).is_some() {
        return true;
    }
    // ponytail: команду делим по пробелам; путь pythonw без пробелов (см. M-CONFIG default).
    //           появятся пробелы в пути — перейти на quote-aware разбор.
    let mut parts = search_cmd.split_whitespace();
    let Some(program) = parts.next() else {
        return false;
    };
    let args: Vec<&str> = parts.collect();
    if std::process::Command::new(program).args(&args).spawn().is_err() {
        return false;
    }
    let start = Instant::now();
    while start.elapsed() < timeout {
        std::thread::sleep(Duration::from_millis(400));
        if health(port).is_some() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_health_maps_model() {
        assert_eq!(parse_health("{\"status\":\"ok\",\"model\":\"ready\"}"), Model::Ready);
        assert_eq!(parse_health("{\"model\":\"loading\"}"), Model::Loading);
        assert_eq!(parse_health("{\"model\":\"cold\"}"), Model::Cold);
        assert_eq!(parse_health("garbage"), Model::Cold);
    }

    #[test]
    fn build_dense_request_shape_and_length() {
        let req = build_dense_request("127.0.0.1:8799", "telegram лайки", "chats", 20);
        assert!(req.starts_with("POST /search HTTP/1.1\r\n"));
        assert!(req.contains("\"mode\":\"dense\""));
        assert!(req.contains("\"scope\":\"chats\""));
        // Content-Length (байты) совпадает с длиной тела
        let body = &req[req.find("\r\n\r\n").unwrap() + 4..];
        let cl: usize = req
            .lines()
            .find_map(|l| l.strip_prefix("Content-Length: "))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(cl, body.len());
    }

    #[test]
    fn build_dense_request_escapes_query() {
        let req = build_dense_request("127.0.0.1:1", "a\"b\\c", "chats", 5);
        assert!(req.contains("\"q\":\"a\\\"b\\\\c\""));
    }
}
