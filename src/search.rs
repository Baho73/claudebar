// FILE: src/search.rs
// VERSION: 1.3.1
// START_MODULE_CONTRACT
//   PURPOSE: Поиск по чатам: живой BM25 нативно (rusqlite поверх своей claudebar_chats.db FTS5, read-only) с агрегацией по project_folder; цвет Bm25 (жёлтый). snippet_for даёт фразу-сниппет для тултипа. Dense отложен (M-SDAEMON dormant). Phase-13.
//   SCOPE: fts_query/aggregate_to_folders/parse_dense_response (чистые), bm25_search/snippet_for (rusqlite), search (BM25-only оркестрация).
//   DEPENDS: none (параметры db/cmd/port приходят из M-CONFIG через M-MAIN; dense-fallback отложен)
//   LINKS: M-SEARCH
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   Color                - цвет подсветки папки: Bm25 (жёлтый) | Dense (синий)
//   FolderHit            - папка-совпадение: folder, color, score
//   fts_query            - чистое: ввод -> FTS5 MATCH (\w+ токены, последний как префикс term*); защита от инъекции
//   bm25_search          - rusqlite: clfind.db (read-only) FTS5 MATCH -> чанки (folder, score)
//   aggregate_to_folders - чистое: чанки -> папки (дедуп по folder, max score), с заданным цветом
//   parse_dense_response - чистое: JSON демона -> папки с dense-скором (наивный разбор; для отложенного dense)
//   snippet_for          - rusqlite: фраза-сниппет (FTS5 snippet) лучшего совпадения в папке (тултип)
//   search               - BM25-only оркестрация (dense отложен)
//   search_bm25          - BM25 по chats_db + (опц.) files_db, слияние по папке (Ф-C «+Файлы»)
//   json_unescape / parse_number_after_colon - приватные помощники разбора
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.3.1 - Phase-13 доводка: snippet_for без фильтра source -> работает и по files_db (сниппет для файловых результатов).
//   v1.3.0 - Phase-13 Ф-C step-7: bm25_search фильтрует source по scope (chats/files); +search_bm25 (chats + опц. files, слияние по папке) для «+Файлы».
//   v1.2.0 - Phase-13 Ф-A step-3: bm25 по своей claudebar_chats.db; +snippet_for (FTS5 snippet для тултипа); search() = BM25-only (dense отложен, M-SDAEMON dormant, импорт sdaemon/Duration убран).
//   v1.1.1 - fix(grace-fix): токен с пунктуацией (IP «187.124.242.233», url, дата) больше не схлопывается в один — words() дробит как unicode61, atom() ищет соседние под-слова фразой. Тест fts_query_operators (IP).
//   v1.1.0 - Phase-12: синтаксис запросов (Google-style) в fts_query — пробел=И, a+b=фраза, a++b=NEAR, -w=исключить, OR=или; sanitize_word против инъекции.
//   v1.0.0 - Phase-12 Step 3: новый модуль поиска. Нативный BM25 (rusqlite по clfind.db) + чистые fts_query/aggregate/parse_dense + оркестрация с dense-fallback (M-SDAEMON).
// END_CHANGE_SUMMARY

use std::collections::HashMap;

use rusqlite::{params, Connection, OpenFlags};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Color {
    Bm25,  // жёлтый: нашли по точным словам
    Dense, // синий: нашли по смыслу (fallback)
}

#[derive(Clone, Debug)]
pub struct FolderHit {
    pub folder: String,
    pub color: Color,
    pub score: f64,
}

// Разбить на \w-под-токены (как делает unicode61: по любым не-буквенно-цифровым),
// нижний регистр. Так IP/url/дата/имя_файла дробятся как в индексе; спецсимволы FTS5
// не проходят (защита от инъекции).
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

// Атом FTS5 из под-слов: одно слово, либо фраза в кавычках (несколько под-слов —
// IP «187.124.242.233» -> "187 124 242 233", соседние). prefix — добавить * (живой набор).
fn atom(ws: &[String], prefix: bool) -> Option<String> {
    match ws.len() {
        0 => None,
        1 => Some(if prefix { format!("{}*", ws[0]) } else { ws[0].clone() }),
        _ => {
            let p = ws.join(" ");
            Some(if prefix { format!("\"{p}\"*") } else { format!("\"{p}\"") })
        }
    }
}

// START_CONTRACT: fts_query
//   PURPOSE: Преобразовать пользовательский ввод (Google-синтаксис) в безопасный FTS5 MATCH.
//   INPUTS: { input: &str }  // пробел=И; a+b=фраза; a++b=NEAR; -w=исключить; OR=или; токен с пунктуацией (IP/url) -> фраза; последнее слово — префикс
//   OUTPUTS: { Option<String> - MATCH-строка или None, если нет позитивных термов }
//   SIDE_EFFECTS: none
// END_CONTRACT: fts_query
pub fn fts_query(input: &str) -> Option<String> {
    let toks: Vec<&str> = input.split_whitespace().collect();
    let n = toks.len();
    let mut pos: Vec<String> = Vec::new(); // позитивные атомы + операторы OR
    let mut excl: Vec<String> = Vec::new(); // исключения (-слово/-фраза)
    for (i, tok) in toks.iter().enumerate() {
        let last = i == n - 1;
        if *tok == "OR" {
            if !pos.is_empty() {
                pos.push("OR".into());
            }
        } else if let Some(rest) = tok.strip_prefix('-') {
            if let Some(a) = atom(&words(rest), false) {
                excl.push(a);
            }
        } else if tok.contains("++") {
            let ws = words(tok);
            match ws.len() {
                0 => {}
                1 => pos.push(ws.into_iter().next().unwrap()),
                _ => pos.push(format!("NEAR({}, 10)", ws.join(" "))),
            }
        } else if tok.contains('+') {
            if let Some(a) = atom(&words(tok), false) {
                pos.push(a);
            }
        } else if let Some(a) = atom(&words(tok), last) {
            // обычный токен; с внутренней пунктуацией (IP/url/дата) станет фразой
            pos.push(a);
        }
    }
    while pos.last().map(|s| s == "OR").unwrap_or(false) {
        pos.pop();
    }
    if pos.is_empty() {
        return None;
    }
    let mut q = pos.join(" ");
    for e in &excl {
        q = format!("({q}) NOT {e}");
    }
    Some(q)
}

// START_CONTRACT: bm25_search
//   PURPOSE: Нативный BM25 по общему индексу clfind.db (read-only).
//   INPUTS: { db_path: &str; query: &str; scope: &str; limit: usize }
//   OUTPUTS: { Vec<(String, f64)> - (project_folder, score), score выше = лучше }
//   SIDE_EFFECTS: открывает clfind.db read-only (FTS5 MATCH); ошибки -> пустой результат
// END_CONTRACT: bm25_search
pub fn bm25_search(db_path: &str, query: &str, scope: &str, limit: usize) -> Vec<(String, f64)> {
    let Some(fts) = fts_query(query) else {
        return Vec::new();
    };
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY;
    let Ok(conn) = Connection::open_with_flags(db_path, flags) else {
        return Vec::new();
    };
    // фильтр источника по scope (контролируемое значение, не пользовательский ввод)
    let src = match scope {
        "files" => " AND c.source='file'",
        "chats" => " AND c.source='chat'",
        _ => "",
    };
    let sql = format!(
        "SELECT c.project_folder, -bm25(chunks_fts) AS score \
         FROM chunks_fts JOIN chunks c ON c.id = chunks_fts.rowid \
         WHERE chunks_fts MATCH ?1{src} \
         ORDER BY bm25(chunks_fts) LIMIT ?2"
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Vec::new();
    };
    let rows = stmt.query_map(params![fts, limit as i64], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
    });
    match rows {
        Ok(it) => it.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    }
}

// START_CONTRACT: aggregate_to_folders
//   PURPOSE: Свести чанки-совпадения к папкам (дедуп, лучший скор), с заданным цветом.
//   INPUTS: { chunks: &[(String, f64)]; color: Color }
//   OUTPUTS: { Vec<FolderHit> - по убыванию скора }
//   SIDE_EFFECTS: none
// END_CONTRACT: aggregate_to_folders
pub fn aggregate_to_folders(chunks: &[(String, f64)], color: Color) -> Vec<FolderHit> {
    let mut best: HashMap<&str, f64> = HashMap::new();
    for (folder, score) in chunks {
        let e = best.entry(folder.as_str()).or_insert(f64::NEG_INFINITY);
        if *score > *e {
            *e = *score;
        }
    }
    let mut out: Vec<FolderHit> = best
        .into_iter()
        .map(|(folder, score)| FolderHit { folder: folder.to_string(), color, score })
        .collect();
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    out
}

fn json_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('/') => out.push('/'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(o) => {
                    out.push('\\');
                    out.push(o);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_number_after_colon(s: &str) -> Option<f64> {
    let s = s.trim_start().strip_prefix(':')?.trim_start();
    let end = s.find(|c: char| c == ',' || c == '}').unwrap_or(s.len());
    s[..end].trim().parse::<f64>().ok() // "null" -> None
}

// START_CONTRACT: parse_dense_response
//   PURPOSE: Достать из JSON-ответа демона папки с непустым dense-скором.
//   INPUTS: { json: &str - {results:[{folder,bm25,dense,best}...]} }
//   OUTPUTS: { Vec<(String, f64)> - (folder, dense_score) для папок с dense != null }
//   SIDE_EFFECTS: none
// END_CONTRACT: parse_dense_response
pub fn parse_dense_response(json: &str) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    let mut rest = json;
    // START_BLOCK_SCAN_RESULTS
    while let Some(fi) = rest.find("\"folder\"") {
        let after_key = &rest[fi + "\"folder\"".len()..];
        let Some(q1) = after_key.find('"') else { break };
        let val = &after_key[q1 + 1..];
        let Some(q2) = val.find('"') else { break };
        let folder = json_unescape(&val[..q2]);
        rest = &val[q2 + 1..];
        // dense этого результата — до следующего "folder"
        let next = rest.find("\"folder\"").unwrap_or(rest.len());
        let seg = &rest[..next];
        if let Some(di) = seg.find("\"dense\"") {
            if let Some(score) = parse_number_after_colon(&seg[di + "\"dense\"".len()..]) {
                out.push((folder, score));
            }
        }
    }
    // END_BLOCK_SCAN_RESULTS
    out
}

// START_CONTRACT: search
//   PURPOSE: BM25-поиск по своей базе (Phase-13). Dense-fallback отложен (M-SDAEMON dormant); cmd/port игнорируются.
//   INPUTS: { db: &str - путь chats_db; cmd: &str - dormant; port: u16 - dormant; query: &str }
//   OUTPUTS: { Vec<FolderHit> - папки (Bm25) }
//   SIDE_EFFECTS: чтение chats_db (FTS5)
// END_CONTRACT: search
pub fn search(db: &str, cmd: &str, port: u16, query: &str) -> Vec<FolderHit> {
    let _ = (cmd, port); // dormant: dense отложен (Phase-13 Ф-A)
    aggregate_to_folders(&bm25_search(db, query, "chats", 200), Color::Bm25)
}

// START_CONTRACT: search_bm25
//   PURPOSE: BM25 по базе чатов + (опц.) базе файлов, слияние по папке (Ф-C «+Файлы»).
//   INPUTS: { chats_db: &str; files_db: Option<&str>; query: &str; limit: usize }
//   OUTPUTS: { Vec<FolderHit> - папки (Bm25), по убыванию }
//   SIDE_EFFECTS: чтение баз read-only
// END_CONTRACT: search_bm25
pub fn search_bm25(chats_db: &str, files_db: Option<&str>, query: &str, limit: usize) -> Vec<FolderHit> {
    let mut chunks = bm25_search(chats_db, query, "chats", limit);
    if let Some(fdb) = files_db {
        chunks.extend(bm25_search(fdb, query, "files", limit));
    }
    aggregate_to_folders(&chunks, Color::Bm25)
}

// START_CONTRACT: snippet_for
//   PURPOSE: Фраза-сниппет лучшего BM25-совпадения в папке (для тултипа чат-результата).
//   INPUTS: { db: &str; query: &str; folder: &str }
//   OUTPUTS: { Option<String> - excerpt (FTS5 snippet) или None }
//   SIDE_EFFECTS: чтение db read-only
// END_CONTRACT: snippet_for
pub fn snippet_for(db: &str, query: &str, folder: &str) -> Option<String> {
    let fts = fts_query(query)?;
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    // без фильтра source: каждая база односорсная (chats=chat, files=file), хватает folder
    let sql = "SELECT snippet(chunks_fts, -1, '', '', '…', 12) \
               FROM chunks_fts JOIN chunks c ON c.id = chunks_fts.rowid \
               WHERE chunks_fts MATCH ?1 AND c.project_folder = ?2 \
               ORDER BY bm25(chunks_fts) LIMIT 1";
    conn.query_row(sql, params![fts, folder], |r| r.get::<_, String>(0)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_query_operators() {
        // И + префикс последнего слова (живой набор)
        assert_eq!(fts_query("telegr").as_deref(), Some("telegr*"));
        assert_eq!(fts_query("окно за экраном").as_deref(), Some("окно за экраном*"));
        // фраза через +
        assert_eq!(fts_query("договор+смета").as_deref(), Some("\"договор смета\""));
        // NEAR через ++
        assert_eq!(fts_query("договор++смета").as_deref(), Some("NEAR(договор смета, 10)"));
        // исключение через -
        assert_eq!(fts_query("смета -черновик").as_deref(), Some("(смета) NOT черновик"));
        // OR
        assert_eq!(fts_query("смета OR накладная").as_deref(), Some("смета OR накладная*"));
        // санитизация: спецсимволы FTS5 срезаются
        assert_eq!(fts_query("a\" b").as_deref(), Some("a b*"));
        // regression: токен с любой пунктуацией -> фраза (соседние), не схлопывание
        assert_eq!(fts_query("187.124.242.233").as_deref(), Some("\"187 124 242 233\"*")); // IP (точки)
        assert_eq!(fts_query("ip 187.124.242.233").as_deref(), Some("ip \"187 124 242 233\"*"));
        assert_eq!(fts_query("12:34:56").as_deref(), Some("\"12 34 56\"*")); // время (двоеточия)
        assert_eq!(fts_query("site.com/path").as_deref(), Some("\"site com path\"*")); // url (слэш)
        // Windows \ и Linux / -> один знаменатель (оба слэша — разделители)
        assert_eq!(fts_query("D:\\Python\\hh").as_deref(), fts_query("D:/Python/hh").as_deref());
        assert_eq!(fts_query("D:\\Python\\hh").as_deref(), Some("\"d python hh\"*"));
        // пусто / только исключение -> None
        assert_eq!(fts_query("   ").as_deref(), None);
        assert_eq!(fts_query("-только").as_deref(), None);
    }

    #[test]
    fn aggregate_dedups_and_colors() {
        let chunks = vec![
            ("A".to_string(), 1.0),
            ("A".to_string(), 3.0),
            ("B".to_string(), 2.0),
        ];
        let hits = aggregate_to_folders(&chunks, Color::Bm25);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].folder, "A"); // max score 3.0 первым
        assert_eq!(hits[0].score, 3.0);
        assert!(hits.iter().all(|h| h.color == Color::Bm25));
    }

    #[test]
    fn parse_dense_skips_null_and_unescapes() {
        let json = r#"{"results":[{"folder":"D:\\Python\\A","bm25":null,"dense":0.7,"best":null},{"folder":"D:\\Python\\B","bm25":null,"dense":null,"best":null}],"model":"ready"}"#;
        let v = parse_dense_response(json);
        assert_eq!(v.len(), 1); // B (dense=null) отброшен
        assert_eq!(v[0].0, "D:\\Python\\A"); // backslashes раз-экранированы
        assert!((v[0].1 - 0.7).abs() < 1e-9);
    }

    #[test]
    fn bm25_search_on_temp_clfind_db() {
        let path = std::env::temp_dir().join("clfind_bm25_test.db");
        let _ = std::fs::remove_file(&path);
        let p = path.to_str().unwrap();
        {
            let conn = Connection::open(p).unwrap();
            conn.execute_batch(
                "CREATE TABLE chunks(id INTEGER PRIMARY KEY, project_folder TEXT, source TEXT, ref TEXT, location TEXT, text TEXT);
                 CREATE VIRTUAL TABLE chunks_fts USING fts5(text, content='chunks', content_rowid='id', tokenize='unicode61');
                 CREATE TRIGGER chunks_ai AFTER INSERT ON chunks BEGIN INSERT INTO chunks_fts(rowid,text) VALUES(new.id,new.text); END;",
            ).unwrap();
            conn.execute("INSERT INTO chunks(project_folder,source,ref,location,text) VALUES (?1,'chat','s','0',?2)",
                params!["D:\\Python\\hh", "обсуждали telegram лайки кандидата"]).unwrap();
            conn.execute("INSERT INTO chunks(project_folder,source,ref,location,text) VALUES (?1,'chat','s','0',?2)",
                params!["D:\\Python\\claudebar", "правим окно за экраном"]).unwrap();
        }
        let hits = bm25_search(p, "telegr", "chats", 10);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].0, "D:\\Python\\hh");
        assert_eq!(bm25_search(p, "экран", "chats", 10)[0].0, "D:\\Python\\claudebar");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn snippet_for_returns_excerpt() {
        let path = std::env::temp_dir().join("clbar_snippet_test.db");
        let _ = std::fs::remove_file(&path);
        let p = path.to_str().unwrap();
        {
            let conn = Connection::open(p).unwrap();
            conn.execute_batch(
                "CREATE TABLE chunks(id INTEGER PRIMARY KEY, project_folder TEXT, source TEXT, ref TEXT, location TEXT, text TEXT);
                 CREATE VIRTUAL TABLE chunks_fts USING fts5(text, content='chunks', content_rowid='id', tokenize='unicode61');
                 CREATE TRIGGER chunks_ai AFTER INSERT ON chunks BEGIN INSERT INTO chunks_fts(rowid,text) VALUES(new.id,new.text); END;",
            ).unwrap();
            conn.execute("INSERT INTO chunks(project_folder,source,ref,location,text) VALUES (?1,'chat','s','0',?2)",
                params!["D:\\Python\\run-pig", "там был подстроечник и конденсатор в схеме"]).unwrap();
        }
        let s = snippet_for(p, "подстроечник", "D:\\Python\\run-pig").unwrap();
        assert!(s.contains("подстроечник"));
        assert!(snippet_for(p, "подстроечник", "D:\\Python\\other").is_none()); // другая папка
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn search_bm25_merges_chats_and_files() {
        let mk = |name: &str, src: &str, folder: &str, text: &str| -> String {
            let path = std::env::temp_dir().join(name);
            let _ = std::fs::remove_file(&path);
            let p = path.to_str().unwrap().to_string();
            let conn = Connection::open(&p).unwrap();
            conn.execute_batch(
                "CREATE TABLE chunks(id INTEGER PRIMARY KEY, project_folder TEXT, source TEXT, ref TEXT, location TEXT, text TEXT);
                 CREATE VIRTUAL TABLE chunks_fts USING fts5(text, content='chunks', content_rowid='id', tokenize='unicode61');
                 CREATE TRIGGER chunks_ai AFTER INSERT ON chunks BEGIN INSERT INTO chunks_fts(rowid,text) VALUES(new.id,new.text); END;",
            ).unwrap();
            conn.execute("INSERT INTO chunks(project_folder,source,ref,location,text) VALUES (?1,?2,'r','0',?3)",
                params![folder, src, text]).unwrap();
            p
        };
        let cdb = mk("clbar_sb_chats.db", "chat", "D:\\A", "смета в чате");
        let fdb = mk("clbar_sb_files.db", "file", "D:\\B", "смета в файле");
        // только чаты -> одна папка
        let only = search_bm25(&cdb, None, "смета", 10);
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].folder, "D:\\A");
        // чаты + файлы -> две папки
        assert_eq!(search_bm25(&cdb, Some(&fdb), "смета", 10).len(), 2);
        let _ = std::fs::remove_file(&cdb);
        let _ = std::fs::remove_file(&fdb);
    }
}
