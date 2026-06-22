// FILE: src/index.rs
// VERSION: 1.0.0
// START_MODULE_CONTRACT
//   PURPOSE: Rust-индексатор BM25: транскрипты Claude Code (~/.claude/projects/**/*.jsonl) -> чанки -> FTS5 claudebar_chats.db. Инкремент по mtime. Своя база (заменяет Python-сборку BM25; clfind остаётся для отложенного dense). Phase-13.
//   SCOPE: init_schema (DDL), parse_transcript/chunk_text (чистые, тестируемые), ensure_index (инкрементальный обход + запись).
//   DEPENDS: M-CONFIG (пути базы и корень индексации приходят параметрами через M-MAIN)
//   LINKS: M-INDEX
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   IndexStats         - итог прогона: files_indexed, files_skipped, chunks_added
//   Transcript         - разобранный транскрипт: project_folder (cwd) + тексты user/assistant
//   init_schema        - DDL: chunks + chunks_fts(unicode61) + files_meta (idempotent)
//   parse_transcript   - чистое: jsonl -> Transcript (cwd + тексты, шум отсеян)
//   chunk_text         - чистое: текст -> чанки ~N слов (хвост не теряется)
//   ensure_index       - инкрементальный обход projects_root, запись новых/изменённых (mtime)
//   message_text / collect_jsonl / index_file / load_meta - приватные помощники
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.0.0 - Phase-13 Ф-A step-1: новый Rust-индексатор. Транскрипты -> FTS5 claudebar_chats.db, инкремент по mtime; чистые parse_transcript/chunk_text + ensure_index (serde_json для парса).
// END_CHANGE_SUMMARY

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde_json::Value;

const CHUNK_WORDS: usize = 512; // ~размер чанка в словах

#[derive(Debug, Default, PartialEq, Eq)]
pub struct IndexStats {
    pub files_indexed: usize,
    pub files_skipped: usize,
    pub chunks_added: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Transcript {
    pub project_folder: String, // из cwd
    pub texts: Vec<String>,     // тексты user/assistant
}

// START_CONTRACT: init_schema
//   PURPOSE: Создать схему индекса (idempotent): chunks + chunks_fts(unicode61) + files_meta.
//   INPUTS: { conn: &Connection }
//   OUTPUTS: { rusqlite::Result<()> }
//   SIDE_EFFECTS: DDL в базе; WAL
// END_CONTRACT: init_schema
pub fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS chunks(
           id INTEGER PRIMARY KEY,
           project_folder TEXT NOT NULL,
           source TEXT NOT NULL,
           ref TEXT,
           location TEXT,
           text TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_chunks_ref ON chunks(ref);
         CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(text, tokenize='unicode61');
         CREATE TABLE IF NOT EXISTS files_meta(path TEXT PRIMARY KEY, mtime INTEGER NOT NULL);",
    )
}

// START_CONTRACT: parse_transcript
//   PURPOSE: Разобрать содержимое jsonl-транскрипта в project_folder (cwd) + тексты user/assistant.
//   INPUTS: { jsonl: &str - всё содержимое файла (несколько JSON-строк) }
//   OUTPUTS: { Option<Transcript> - None если нет cwd или нет текста }
//   SIDE_EFFECTS: none (битые/незнакомые строки пропускаются)
// END_CONTRACT: parse_transcript
pub fn parse_transcript(jsonl: &str) -> Option<Transcript> {
    let mut project_folder: Option<String> = None;
    let mut texts: Vec<String> = Vec::new();
    // START_BLOCK_SCAN_LINES
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue; // битая строка — skip, не паника
        };
        if project_folder.is_none() {
            if let Some(cwd) = v.get("cwd").and_then(|c| c.as_str()) {
                project_folder = Some(cwd.to_string());
            }
        }
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if ty != "user" && ty != "assistant" {
            continue; // только реплики, не tool/summary/служебное
        }
        if let Some(t) = v.get("message").and_then(message_text) {
            if !t.trim().is_empty() {
                texts.push(t);
            }
        }
    }
    // END_BLOCK_SCAN_LINES
    let project_folder = project_folder?;
    if texts.is_empty() {
        return None;
    }
    Some(Transcript { project_folder, texts })
}

// Текст из message.content: строка (user) или массив частей {type:"text", text}.
// tool_use/tool_result (шум) отсеиваются.
fn message_text(msg: &Value) -> Option<String> {
    let content = msg.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    let arr = content.as_array()?;
    let mut out = String::new();
    for part in arr {
        if part.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

// START_CONTRACT: chunk_text
//   PURPOSE: Нарезать текст на чанки ~max_words слов (по словам, хвост не теряется).
//   INPUTS: { text: &str; max_words: usize }
//   OUTPUTS: { Vec<String> }
//   SIDE_EFFECTS: none
// END_CONTRACT: chunk_text
pub fn chunk_text(text: &str, max_words: usize) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    words.chunks(max_words.max(1)).map(|c| c.join(" ")).collect()
}

// START_CONTRACT: ensure_index
//   PURPOSE: Инкрементально проиндексировать новые/изменённые транскрипты (по mtime) в claudebar_chats.db.
//   INPUTS: { chats_db: &str; projects_root: &str }
//   OUTPUTS: { IndexStats }
//   SIDE_EFFECTS: создаёт/пишет claudebar_chats.db (chunks/chunks_fts/files_meta); ошибки -> пропуск
// END_CONTRACT: ensure_index
pub fn ensure_index(chats_db: &str, projects_root: &str) -> IndexStats {
    let mut stats = IndexStats::default();
    let Ok(mut conn) = Connection::open(chats_db) else {
        return stats;
    };
    if init_schema(&conn).is_err() {
        return stats;
    }
    let known = load_meta(&conn);
    let mut files = Vec::new();
    collect_jsonl(Path::new(projects_root), &mut files);
    // START_BLOCK_INCREMENTAL
    for path in files {
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let key = path.to_string_lossy().to_string();
        if known.get(&key) == Some(&mtime) {
            stats.files_skipped += 1;
            continue; // не изменился — пропуск
        }
        match index_file(&mut conn, &key, &path, mtime) {
            Some(n) => {
                stats.files_indexed += 1;
                stats.chunks_added += n;
            }
            None => stats.files_skipped += 1,
        }
    }
    // END_BLOCK_INCREMENTAL
    stats
}

fn load_meta(conn: &Connection) -> HashMap<String, i64> {
    let mut m = HashMap::new();
    if let Ok(mut stmt) = conn.prepare("SELECT path, mtime FROM files_meta") {
        if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))) {
            for r in rows.flatten() {
                m.insert(r.0, r.1);
            }
        }
    }
    m
}

// Рекурсивный сбор *.jsonl.
fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_jsonl(&p, out);
        } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
            out.push(p);
        }
    }
}

// Проиндексировать один файл: удалить старые чанки по ref, вставить новые, обновить mtime.
fn index_file(conn: &mut Connection, key: &str, path: &Path, mtime: i64) -> Option<usize> {
    let content = fs::read_to_string(path).ok()?;
    let t = parse_transcript(&content)?;
    let tx = conn.transaction().ok()?;
    let _ = tx.execute(
        "DELETE FROM chunks_fts WHERE rowid IN (SELECT id FROM chunks WHERE ref=?1)",
        params![key],
    );
    let _ = tx.execute("DELETE FROM chunks WHERE ref=?1", params![key]);
    let mut n = 0;
    for text in &t.texts {
        for chunk in chunk_text(text, CHUNK_WORDS) {
            if tx
                .execute(
                    "INSERT INTO chunks(project_folder, source, ref, location, text) VALUES(?1,'chat',?2,NULL,?3)",
                    params![t.project_folder, key, chunk],
                )
                .is_ok()
            {
                let id = tx.last_insert_rowid();
                let _ = tx.execute("INSERT INTO chunks_fts(rowid, text) VALUES(?1, ?2)", params![id, chunk]);
                n += 1;
            }
        }
    }
    let _ = tx.execute(
        "INSERT OR REPLACE INTO files_meta(path, mtime) VALUES(?1, ?2)",
        params![key, mtime],
    );
    tx.commit().ok()?;
    Some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_transcript_extracts_cwd_and_texts() {
        let jsonl = concat!(
            r#"{"type":"user","cwd":"D:/Python/run-pig","message":{"role":"user","content":"вопрос про подстроечник"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ответ про конденсатор"},{"type":"tool_use","name":"x","input":{}}]}}"#,
            "\n",
            "не-json мусор\n",
            r#"{"type":"summary","summary":"x"}"#
        );
        let t = parse_transcript(jsonl).unwrap();
        assert_eq!(t.project_folder, "D:/Python/run-pig");
        assert_eq!(t.texts, vec!["вопрос про подстроечник", "ответ про конденсатор"]);
    }

    #[test]
    fn parse_transcript_none_without_cwd_or_text() {
        // нет cwd
        assert!(parse_transcript(r#"{"type":"user","message":{"content":"hi"}}"#).is_none());
        // только мусор/служебное
        assert!(parse_transcript("мусор\n{\"type\":\"summary\"}").is_none());
    }

    #[test]
    fn chunk_text_splits_by_words_keeps_tail() {
        let text = (1..=1100).map(|i| i.to_string()).collect::<Vec<_>>().join(" ");
        let chunks = chunk_text(&text, 512);
        assert_eq!(chunks.len(), 3); // 512 + 512 + 76
        assert!(chunks[0].starts_with("1 2 3"));
        assert!(chunks[2].ends_with("1100"));
        assert_eq!(chunk_text("   ", 512), Vec::<String>::new());
    }

    #[test]
    fn ensure_index_indexes_and_is_incremental() {
        let dir = std::env::temp_dir().join(format!("clbar_idx_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let proj = dir.join("D--Python-run-pig");
        std::fs::create_dir_all(&proj).unwrap();
        let line = r#"{"type":"user","cwd":"D:/Python/run-pig","message":{"content":"слово подстроечник тут"}}"#;
        std::fs::write(proj.join("sess.jsonl"), format!("{line}\n")).unwrap();
        let dbs = dir.join("chats.db").to_string_lossy().to_string();
        let root = dir.to_string_lossy().to_string();

        let s1 = ensure_index(&dbs, &root);
        assert_eq!(s1.files_indexed, 1);
        assert!(s1.chunks_added >= 1);

        let conn = Connection::open(&dbs).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH 'подстроечник'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        drop(conn);

        // повторный прогон без изменений — файл пропущен (инкремент по mtime)
        let s2 = ensure_index(&dbs, &root);
        assert_eq!(s2.files_indexed, 0);
        assert_eq!(s2.files_skipped, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
