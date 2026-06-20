# clfind MVP (поиск по чатам) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Локальный поисковый движок по транскриптам Claude Code: индекс (BM25 + dense на Qwen3-4B) и HTTP-демон, отдающий папки-совпадения с раздельными флагами bm25/dense.

**Architecture:** Python-пакет `clfind` в новом репозитории `D:\Python\clfind`. Чистые модули (parse/chunk) тестируются на фикстурах; SQLite FTS5 = BM25; FAISS = dense; модель грузится лениво (prefetch/evict); FastAPI-демон отдаёт `/warmup /search /health`. Поиск НЕ сливает ранги (без RRF) — отдаёт per-folder `bm25`/`dense` скоры, цвет считает ClaudeBar.

**Tech Stack:** Python 3.11+, sentence-transformers (Qwen3-Embedding-4B), faiss-cpu, SQLite/FTS5 (stdlib), FastAPI+uvicorn, pytest. Энкод индекса на CUDA, энкод запроса на CPU.

## Global Constraints
- Платформа: Windows, 2×RTX 3060 12ГБ, 64ГБ RAM. Индекс-энкод `cuda`, query-энкод `cpu`.
- Источник: только `~/.claude/projects/*/*.jsonl`. `project_folder` берётся из поля `cwd` строки (НЕ из имени каталога).
- Модель = строка конфига, дефолт `Qwen/Qwen3-Embedding-4B` (переключаемо).
- Поиск без RRF: ответ на папку = `{folder, bm25:<score|null>, dense:<score|null>, best}`.
- Кодировка UTF-8 явно везде (читаем jsonl с `encoding="utf-8"`).
- TDD, частые коммиты, DRY, YAGNI. MVP = только чаты (файлы/Office — Фаза 2+, не здесь).

## Файловая структура (clfind MVP)
```
D:\Python\clfind\
  pyproject.toml            # deps + console-scripts
  clfind\__init__.py
  clfind\config.py          # Config (TOML), пути индекса
  clfind\transcripts.py     # parse_transcript(lines)->Record[] (чистое)
  clfind\chunking.py        # chunk_text(text)->list[str] (чистое)
  clfind\store.py           # Store: SQLite + FTS5, add_chunk, bm25_search, manifest
  clfind\embedder.py        # Embedder: ленивая модель, encode_query/encode_docs, state
  clfind\vectors.py         # VectorIndex: FAISS IndexIDMap2, add/search/save
  clfind\indexer.py         # index_chats(...) инкремент по mtime
  clfind\search.py          # search(...)->FolderResult[] (per-folder bm25/dense)
  clfind\daemon.py          # FastAPI: /warmup /search /health
  clfind\cli.py             # `clfind index`, `clfind serve`
  tests\...                 # pytest на каждый чистый модуль
```
Граница с ClaudeBar — только HTTP-контракт (Track 2, отдельный grace-plan).

---

### Task 1: Каркас проекта + Config

**Files:**
- Create: `D:\Python\clfind\pyproject.toml`, `clfind\__init__.py`, `clfind\config.py`
- Test: `tests\test_config.py`

**Interfaces:**
- Produces: `Config` (dataclass) с полями `model,port,projects_dir,data_dir,roots,index_device,query_device,batch_size,idle_evict_sec,live_min_chars`; `Config.load(path)->Config` (TOML, отсутствующий файл → дефолты).

- [ ] **Step 1: Тест** — `tests\test_config.py`
```python
from clfind.config import Config
def test_defaults_when_no_file(tmp_path):
    c = Config.load(tmp_path / "nope.toml")
    assert c.model == "Qwen/Qwen3-Embedding-4B"
    assert c.index_device == "cuda" and c.query_device == "cpu"
    assert c.live_min_chars == 3
def test_toml_overrides(tmp_path):
    p = tmp_path / "c.toml"; p.write_text('model="BAAI/bge-m3"\nport=9001\n', encoding="utf-8")
    c = Config.load(p)
    assert c.model == "BAAI/bge-m3" and c.port == 9001
```
- [ ] **Step 2:** `pytest tests/test_config.py -v` → FAIL (нет модуля).
- [ ] **Step 3:** реализовать `Config` (dataclass + `tomllib`; дефолты; `data_dir`/`projects_dir` от `Path.home()`).
- [ ] **Step 4:** `pytest tests/test_config.py -v` → PASS.
- [ ] **Step 5:** `git init` + commit `feat: project scaffold + config`.

---

### Task 2: Парсер транскриптов

**Files:** Create `clfind\transcripts.py`; Test `tests\test_transcripts.py` + фикстура `tests\fixtures\mini.jsonl`.

**Interfaces:**
- Produces: `Record(project_folder:str, session_id:str, role:str, text:str, ts:str|None)`; `parse_transcript(lines: Iterable[str], session_id: str) -> Iterator[Record]`.
- Логика: `json.loads` строки; запоминать последний `cwd`; брать `type in {user,assistant}` где `message.content` это `str` или список блоков с `type=="text"` (склеить); отсев `tool_use/tool_result/attachment/file-history-snapshot`, пустых, служебных.

- [ ] **Step 1: Тест** (фикстура — 6 строк: cwd-несущая user, assistant-с-text-блоками, tool_result, attachment, snapshot, пустой assistant):
```python
from clfind.transcripts import parse_transcript
def test_extracts_user_assistant_with_cwd(tmp_path):
    lines = [
      '{"type":"user","cwd":"D:\\\\Python\\\\hh-answer","message":{"role":"user","content":"где telegram лайки"}}',
      '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"вот тут"},{"type":"tool_use","name":"x"}]}}',
      '{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"noise"}]}}',
      '{"type":"file-history-snapshot","snapshot":{}}',
    ]
    recs = list(parse_transcript(lines, "sess1"))
    assert [r.role for r in recs] == ["user","assistant"]
    assert recs[0].project_folder == "D:\\Python\\hh-answer"
    assert recs[0].text == "где telegram лайки"
    assert recs[1].text == "вот тут"           # tool_use-блок отброшен
    assert recs[1].project_folder == "D:\\Python\\hh-answer"  # cwd протянут
```
- [ ] **Step 2:** `pytest tests/test_transcripts.py -v` → FAIL.
- [ ] **Step 3:** реализовать `parse_transcript` (см. логику выше; `content` str → как есть; list → join текстов блоков `type=="text"`; cwd из любой строки, где есть, держать «последним известным»; skip если итоговый текст пуст).
- [ ] **Step 4:** PASS.
- [ ] **Step 5:** commit `feat: transcript parser (cwd + text extraction)`.

---

### Task 3: Чанкинг

**Files:** Create `clfind\chunking.py`; Test `tests\test_chunking.py`.

**Interfaces:**
- Produces: `chunk_text(text: str, max_words=400, overlap=64) -> list[str]`. Окно по словам (детерминированно, без токенайзера), перекрытие; короткий текст → один чанк.

- [ ] **Step 1: Тест**
```python
from clfind.chunking import chunk_text
def test_short_text_single_chunk():
    assert chunk_text("a b c") == ["a b c"]
def test_windows_with_overlap():
    words = " ".join(str(i) for i in range(900))
    chunks = chunk_text(words, max_words=400, overlap=64)
    assert len(chunks) >= 2
    assert chunks[0].split()[:1] == ["0"]
    # перекрытие: конец 1-го чанка повторяется в начале 2-го
    assert chunks[0].split()[-1] in chunks[1].split()[:64]
```
- [ ] **Step 2:** FAIL. — [ ] **Step 3:** реализовать (слайдинг `max_words` со сдвигом `max_words-overlap`). — [ ] **Step 4:** PASS. — [ ] **Step 5:** commit `feat: word-window chunking`.

---

### Task 4: SQLite Store + BM25 (FTS5)

**Files:** Create `clfind\store.py`; Test `tests\test_store.py`.

**Interfaces:**
- Produces: `Store(db_path)` (создаёт схему + FTS5); `add_chunk(project_folder, source, ref, location, text) -> int` (chunk id); `source_mtime(source_key)->float|None`; `set_source_mtime(source_key, mtime)`; `delete_source(source_key)`; `bm25_search(query, scope, limit) -> list[Hit]` где `Hit(id,project_folder,source,ref,location,score)`. Последний токен запроса трактуется как префикс `tok*`.
- Схема: `chunks(id INTEGER PK, project_folder, source, ref, location, text)`, FTS5 `chunks_fts(text, content='chunks', content_rowid='id')`, `manifest(source_key PK, mtime REAL)`.

- [ ] **Step 1: Тест**
```python
from clfind.store import Store
def test_add_and_bm25_prefix(tmp_path):
    s = Store(tmp_path / "i.db")
    s.add_chunk("D:\\Python\\hh-answer", "chat", "sess1", "0", "обсуждали telegram лайки кандидата")
    s.add_chunk("D:\\Python\\claudebar", "chat", "sess2", "0", "правим окно за экраном")
    hits = s.bm25_search("telegr", scope="chats", limit=10)   # префикс
    assert hits and hits[0].project_folder == "D:\\Python\\hh-answer"
    assert s.bm25_search("экран", "chats", 10)[0].project_folder == "D:\\Python\\claudebar"
def test_manifest_roundtrip(tmp_path):
    s = Store(tmp_path / "i.db")
    assert s.source_mtime("sess1") is None
    s.set_source_mtime("sess1", 123.0); assert s.source_mtime("sess1") == 123.0
```
- [ ] **Step 2:** FAIL. — [ ] **Step 3:** реализовать (sqlite3, FTS5-таблица с триггерами на insert/delete для синхронизации content; `bm25_search`: разбить запрос на токены, последний → `tok*`, `MATCH`, `ORDER BY bm25(chunks_fts)`; `delete_source` чистит chunks+fts). — [ ] **Step 4:** PASS. — [ ] **Step 5:** commit `feat: sqlite store + fts5 bm25`.

---

### Task 5: Embedder (ленивая модель)

**Files:** Create `clfind\embedder.py`; Test `tests\test_embedder.py`.

**Interfaces:**
- Produces: `Embedder(model_name, index_device, query_device, loader=None)`; свойство `state -> "cold"|"loading"|"ready"`; `warmup()` (запускает загрузку, идемпотентно); `ensure_ready()` (блокирующая загрузка); `evict()`; `encode_query(text)->np.ndarray` (1×dim, L2-norm, query-instruction); `encode_docs(list[str])->np.ndarray` (n×dim, L2-norm). `loader` инъектируем для тестов (по умолчанию грузит SentenceTransformer).
- Примечание реализации: query-instruction по Qwen3 (`"Instruct: ...\nQuery: <q>"`), docs без инструкции; `query_device` для encode_query, `index_device` для encode_docs.

- [ ] **Step 1: Тест** (с фейковым loader — без реальной модели):
```python
import numpy as np
from clfind.embedder import Embedder
class FakeModel:
    def encode(self, texts, **kw):
        v = np.ones((len(texts), 4), dtype="float32"); return v
def test_lazy_state_and_evict():
    e = Embedder("x", "cpu", "cpu", loader=lambda name, dev: FakeModel())
    assert e.state == "cold"
    e.ensure_ready(); assert e.state == "ready"
    q = e.encode_query("hi"); assert q.shape == (1, 4)
    assert np.allclose(np.linalg.norm(q), 1.0)   # нормализовано
    e.evict(); assert e.state == "cold"
```
- [ ] **Step 2:** FAIL. — [ ] **Step 3:** реализовать (state-машина; `warmup` в фоне через `threading.Thread`; `ensure_ready` блокирует; нормализация; loader по умолчанию `lambda name,dev: SentenceTransformer(name, device=dev)`). — [ ] **Step 4:** PASS. — [ ] **Step 5:** commit `feat: lazy embedder with evict`.

---

### Task 6: VectorIndex (FAISS)

**Files:** Create `clfind\vectors.py`; Test `tests\test_vectors.py`.

**Interfaces:**
- Produces: `VectorIndex(path, dim)` (грузит файл, если есть; иначе `IndexIDMap2(IndexFlatIP(dim))`); `add(ids: np.ndarray[int64], vecs: np.ndarray[float32])`; `search(qvec, k)->list[tuple[int,float]]` (chunk_id, score); `remove(ids)`; `save()`.

- [ ] **Step 1: Тест** (синтетика, без модели):
```python
import numpy as np
from clfind.vectors import VectorIndex
def test_add_search_persist(tmp_path):
    p = tmp_path / "v.faiss"; vi = VectorIndex(p, dim=4)
    vi.add(np.array([10,20], dtype="int64"),
           np.array([[1,0,0,0],[0,1,0,0]], dtype="float32"))
    vi.save()
    vi2 = VectorIndex(p, dim=4)
    hit = vi2.search(np.array([[1,0,0,0]], dtype="float32"), k=1)
    assert hit[0][0] == 10
```
- [ ] **Step 2:** FAIL. — [ ] **Step 3:** реализовать (faiss IndexIDMap2 поверх IndexFlatIP; `add_with_ids`; `search` → (id,score); `write_index`/`read_index`). — [ ] **Step 4:** PASS. — [ ] **Step 5:** commit `feat: faiss vector index`.

---

### Task 7: Indexer (инкремент по mtime)

**Files:** Create `clfind\indexer.py`; добавить `clfind index` в `clfind\cli.py`; Test `tests\test_indexer.py`.

**Interfaces:**
- Consumes: Config, Store, VectorIndex, Embedder, parse_transcript, chunk_text.
- Produces: `index_chats(cfg, store, vectors, embedder, *, reindex=False) -> dict` (stats: `{files, chunks, skipped}`). Скан `cfg.projects_dir/*/*.jsonl`; `source_key=session_id` (имя файла без .jsonl); mtime-проверка через `store.source_mtime`; изменённый источник: `delete_source` → parse → chunk → `embedder.encode_docs` → `store.add_chunk` + `vectors.add(id, vec)`; `set_source_mtime`.

- [ ] **Step 1: Тест** (tmp projects-dir с 1 мини-jsonl, фейковый embedder из Task 5):
```python
import numpy as np
from clfind.indexer import index_chats
# ... собрать tmp cfg.projects_dir/proj/sess1.jsonl с 2 user/assistant строками
def test_incremental_index(tmp_path, monkeypatch):
    # первый прогон индексирует, второй (без изменений) пропускает
    stats1 = index_chats(cfg, store, vectors, fake_embedder)
    assert stats1["chunks"] >= 1
    stats2 = index_chats(cfg, store, vectors, fake_embedder)
    assert stats2["chunks"] == 0 and stats2["skipped"] >= 1
```
- [ ] **Step 2:** FAIL. — [ ] **Step 3:** реализовать `index_chats` + CLI `clfind index`. — [ ] **Step 4:** PASS. — [ ] **Step 5:** commit `feat: incremental chat indexer + cli`.

---

### Task 8: Search (per-folder, без RRF)

**Files:** Create `clfind\search.py`; Test `tests\test_search.py`.

**Interfaces:**
- Produces: `FolderResult(folder, bm25:float|None, dense:float|None, best:dict|None)`; `search(store, vectors, embedder, q, scope, mode, limit) -> list[FolderResult]`. `mode="bm25"` → только BM25; `mode="full"` → BM25 + (encode_query→vectors.search→chunk_id→folder через store). Агрегация по folder: max bm25, max dense, флаги; `best` = чанк с лучшим скором (source/ref/location); сортировка: оба-сначала, затем по max-скору.

- [ ] **Step 1: Тест** (monkeypatch `store.bm25_search` и `vectors.search`/`embedder`):
```python
from clfind.search import search
def test_folder_tagging(monkeypatch, fake_store, fake_vectors, fake_embedder):
    # bm25 находит folderA; dense находит folderA и folderB
    res = {r.folder: r for r in search(fake_store, fake_vectors, fake_embedder,
                                       "q", scope="chats", mode="full", limit=10)}
    assert res["A"].bm25 is not None and res["A"].dense is not None   # оба -> зелёный
    assert res["B"].bm25 is None and res["B"].dense is not None       # только dense -> синий
def test_bm25_mode_skips_dense(monkeypatch, ...):
    res = search(..., mode="bm25", ...)
    assert all(r.dense is None for r in res)
```
- [ ] **Step 2:** FAIL. — [ ] **Step 3:** реализовать (см. interfaces; для `full` маппинг chunk_id→(folder, ref) брать из store по id). — [ ] **Step 4:** PASS. — [ ] **Step 5:** commit `feat: per-folder search (bm25/dense flags, no rrf)`.

---

### Task 9: HTTP-демон (FastAPI)

**Files:** Create `clfind\daemon.py`; добавить `clfind serve` в `clfind\cli.py`; Test `tests\test_daemon.py`.

**Interfaces:**
- Эндпоинты: `GET /health` → `{status:"ok", model: embedder.state}`; `POST /warmup` → `embedder.warmup()`, `{model: state}`; `POST /search {q, scope="chats", mode="bm25"|"full", limit=20}` → `{results:[{folder,bm25,dense,best}], model: state}`.
- Демон держит Store/VectorIndex/Embedder; idle-таймер вызывает `embedder.evict()` после `idle_evict_sec`.

- [ ] **Step 1: Тест** (`fastapi.testclient.TestClient`, поиск замокан):
```python
from fastapi.testclient import TestClient
from clfind.daemon import make_app
def test_health_and_search(monkeypatch):
    app = make_app(cfg, fake_store, fake_vectors, fake_embedder)
    c = TestClient(app)
    assert c.get("/health").json()["status"] == "ok"
    r = c.post("/search", json={"q":"telegr","scope":"chats","mode":"bm25"}).json()
    assert "results" in r and isinstance(r["results"], list)
    assert c.post("/warmup").json()["model"] in ("loading","ready")
```
- [ ] **Step 2:** FAIL. — [ ] **Step 3:** реализовать `make_app(...)` + `clfind serve` (uvicorn). — [ ] **Step 4:** PASS. — [ ] **Step 5:** commit `feat: fastapi daemon (warmup/search/health)`.

---

### Task 10: Боевой прогон + smoke (ручной)

- [ ] Установить deps (`pip install -e .`), скачать Qwen3-Embedding-4B (первый запуск).
- [ ] `clfind index` на реальных `~/.claude/projects` (засечь время; ожидаемо ~1–2ч). Проверить размер `clfind.db`/`clfind.faiss`, число чанков.
- [ ] `clfind serve`; `curl` `/health`, `/warmup`, затем `/search` на кейсе «telegram-ник» (mode=bm25 мгновенно; mode=full даёт dense). Глазами проверить, что папки-совпадения осмысленны.
- [ ] commit `chore: first real index run notes` (зафиксировать тайминги/число чанков в README).

---

## Self-Review (по спеку)
- **Покрытие:** парсер(2)+cwd ✓, чанкинг(3) ✓, BM25/FTS5(4) ✓, dense/FAISS(6) ✓, Qwen3-4B cuda-index/cpu-query + ленивая загрузка(5) ✓, инкремент по mtime(7) ✓, /warmup /search(mode) /health(9) ✓, per-folder bm25/dense без RRF(8) ✓. Live-поиск с 3-го символа и цвет — на стороне ClaudeBar (Track 2), движок лишь поддерживает `mode=bm25` и раздельные флаги ✓. GPU-планировщик/файлы/Office/jump — Фазы 2-4, вне MVP ✓.
- **Плейсхолдеры:** нет TBD; «боевой прогон» (Task 10) — намеренно ручной (модель тяжёлая).
- **Типы:** `Hit`(store)↔search; `FolderResult` единый; `Embedder.state` строки совпадают с `/health`/`/search` ответом.

## Execution Handoff
План сохранён: `docs/superpowers/plans/2026-06-20-clfind-mvp.md`.
Track 2 (ClaudeBar M-SEARCH) — отдельный план через `grace-plan` ПОСЛЕ Task 9 (когда HTTP-контракт зафиксирован).
