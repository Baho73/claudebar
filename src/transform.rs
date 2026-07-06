// FILE: src/transform.rs
// VERSION: 1.2.0
// START_MODULE_CONTRACT
//   PURPOSE: Пост-обработка распознанного текста перед вставкой: чистка типового мусора Whisper + кастомный словарь пост-заменой.
//   SCOPE: clean_whisper (теги в скобках, повторы слов, галлюцинации-предложения, капитализация), apply_vocab (замена по словарю по границе слова, регистронезависимо), process (связка + опц. хвостовой пробел), stable_prefix/merge_committed (стриминг: устоявшийся префикс + склейка). Всё чистое.
//   DEPENDS: none
//   LINKS: M-TRANSFORM
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   clean_whisper - чистое: убрать [теги], схлопнуть повторы слов, выкинуть предложения-галлюцинации, капитализировать
//   apply_vocab   - чистое: замена слов по словарю wrong->right (регистронезависимо, по границе слова)
//   process       - чистое: clean_whisper -> apply_vocab -> опц. хвостовой пробел
//   stable_prefix - чистое: устоявшийся общий префикс двух распознаваний (LocalAgreement стриминга)
//   merge_committed - чистое: склейка зафиксированного текста и хвоста одним пробелом
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.2.0 - Phase-24: чистые stable_prefix(prev,cur,margin) (устоявшийся общий префикс окна — LocalAgreement) и merge_committed(fixed,tail) (склейка без двойных пробелов) для стриминг-диктовки. Тесты stable_prefix_commits_agreed_words/merge_committed_no_double_space.
//   v1.1.0 - Phase-23: process += trailing_space (опц. хвостовой пробел, чтобы следующая вставка диктовки не липла к точке). M-VOICE worker передаёт cfg.voice_trailing_space.
//   v1.0.0 - Phase-18 step-4: чистка вывода Whisper (список галлюцинаций на тишине/шуме,
//                повторы, скобочные теги, капитализация) + словарь пост-замены (модель не держит hotwords,
//                словарь делаем текстом). Внешний transform (переформ./перевод) — Phase-D, тут шва нет (YAGNI).
// END_CHANGE_SUMMARY

// Типовые галлюцинации Whisper на тишине/шуме/музыке (нормализованные: нижний регистр, без пунктуации).
// Сверка по равенству или префиксу нормализованного предложения.
const JUNK: &[&str] = &[
    "продолжение следует",
    "спасибо за просмотр",
    "спасибо за внимание",
    "субтитры подготовил",
    "субтитры сделал",
    "субтитры делал",
    "субтитры создавал",
    "редактор субтитров",
    "субтитры subtitles",
    "подписывайтесь на канал",
    "подписывайтесь",
    "ставьте лайки",
    "ставьте лайк",
    "дякую за перегляд",
    "продолжение в следующей серии",
];

// Убрать содержимое квадратных скобок ([музыка], [аплодисменты]) — это звуковые теги Whisper.
fn strip_brackets(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0u32;
    for c in s.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

// Нормализовать предложение и проверить, не является ли оно галлюцинацией.
fn is_junk(sentence: &str) -> bool {
    let norm: String = sentence
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c.is_whitespace() { c } else { ' ' })
        .collect();
    let norm = norm.split_whitespace().collect::<Vec<_>>().join(" ");
    if norm.is_empty() {
        return false;
    }
    JUNK.iter().any(|j| norm == *j || norm.starts_with(j))
}

// Разбить на предложения по терминаторам . ? ! (терминатор остаётся в предложении).
fn split_sentences(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        cur.push(c);
        if matches!(c, '.' | '?' | '!') {
            let t = cur.trim();
            if !t.is_empty() {
                out.push(t.to_string());
            }
            cur.clear();
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        out.push(t.to_string());
    }
    out
}

// Заглавная буква в начале строки и после терминаторов предложений.
fn capitalize_sentences(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut cap = true;
    for c in s.chars() {
        if cap && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            cap = false;
        } else {
            out.push(c);
            if matches!(c, '.' | '?' | '!') {
                cap = true;
            }
        }
    }
    out
}

// START_CONTRACT: clean_whisper
//   PURPOSE: Вычистить типовой мусор распознавания Whisper, оставив осмысленный текст.
//   INPUTS: { text: &str - сырой вывод распознавания }
//   OUTPUTS: { String - очищенный текст; "" если весь ввод оказался мусором/тишиной }
//   SIDE_EFFECTS: none
// END_CONTRACT: clean_whisper
pub fn clean_whisper(text: &str) -> String {
    let s = strip_brackets(text);
    // схлопнуть подряд идущие дубли слов (whisper-залипания), одиночные пробелы
    let mut words: Vec<&str> = Vec::new();
    for w in s.split_whitespace() {
        if words.last().map(|p| p.to_lowercase() == w.to_lowercase()) != Some(true) {
            words.push(w);
        }
    }
    let joined = words.join(" ");
    // выкинуть предложения-галлюцинации и обрывки из одной пунктуации (целиком/хвост/начало)
    let kept: Vec<String> = split_sentences(&joined)
        .into_iter()
        .filter(|snt| snt.chars().any(|c| c.is_alphanumeric()) && !is_junk(snt))
        .collect();
    capitalize_sentences(kept.join(" ").trim())
}

// START_CONTRACT: apply_vocab
//   PURPOSE: Заменить слова по кастомному словарю (имена, термины, латиница) — модель не держит hotwords.
//   INPUTS: { text: &str; vocab: &[(String wrong, String right)] }
//   OUTPUTS: { String - текст с заменами по границе слова, регистронезависимо }
//   SIDE_EFFECTS: none
// END_CONTRACT: apply_vocab
pub fn apply_vocab(text: &str, vocab: &[(String, String)]) -> String {
    if vocab.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if word.is_empty() {
            return;
        }
        let wl = word.to_lowercase();
        match vocab.iter().find(|(w, _)| w.to_lowercase() == wl) {
            Some((_, r)) => out.push_str(r),
            None => out.push_str(word),
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_alphanumeric() {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

// START_CONTRACT: process
//   PURPOSE: Полная пост-обработка перед вставкой: чистка мусора, словарь, опц. хвостовой пробел.
//   INPUTS: { text: &str; vocab: &[(String, String)]; trailing_space: bool }
//   OUTPUTS: { String - готовый к вставке текст; при trailing_space и непустом результате — пробел в хвосте (чтобы следующая вставка не липла к точке) }
//   SIDE_EFFECTS: none
// END_CONTRACT: process
pub fn process(text: &str, vocab: &[(String, String)], trailing_space: bool) -> String {
    let out = apply_vocab(&clean_whisper(text), vocab);
    if trailing_space && !out.is_empty() {
        out + " "
    } else {
        out
    }
}

// START_CONTRACT: stable_prefix
//   PURPOSE: Устоявшийся общий префикс двух последовательных распознаваний окна (LocalAgreement): слова, совпавшие
//            в prev и cur с начала, за вычетом последних margin_words плавающего хвоста cur (страховка от дрейфа).
//   INPUTS: { prev: &str - прошлое распознавание; cur: &str - текущее; margin_words: usize - запас хвоста }
//   OUTPUTS: { String - зафиксированный префикс (слова через пробел); "" если ничего не устоялось }
//   SIDE_EFFECTS: none
//   LINKS: M-VOICE (накопление committed_text при стриминге)
// END_CONTRACT: stable_prefix
pub fn stable_prefix(prev: &str, cur: &str, margin_words: usize) -> String {
    let pw: Vec<&str> = prev.split_whitespace().collect();
    let cw: Vec<&str> = cur.split_whitespace().collect();
    // длина общего префикса по словам
    let mut common = 0;
    while common < pw.len() && common < cw.len() && pw[common] == cw[common] {
        common += 1;
    }
    // не фиксировать слова из последних margin_words позиций cur (близко к концу окна -> ещё нестабильны)
    let limit = cw.len().saturating_sub(margin_words);
    let take = common.min(limit);
    cw[..take].join(" ")
}

// START_CONTRACT: merge_committed
//   PURPOSE: Склеить зафиксированный текст и хвост ровно одним пробелом на стыке (без двойных пробелов).
//   INPUTS: { fixed: &str - уже зафиксировано; tail: &str - добавляемый хвост }
//   OUTPUTS: { String - fixed + " " + tail без лишних пробелов на стыке }
//   SIDE_EFFECTS: none
//   LINKS: M-VOICE (финальная склейка на стопе)
// END_CONTRACT: merge_committed
pub fn merge_committed(fixed: &str, tail: &str) -> String {
    let t = tail.trim_start();
    if t.is_empty() {
        return fixed.to_string(); // пустой хвост -> fixed без изменений
    }
    let f = fixed.trim_end();
    if f.is_empty() {
        return t.to_string();
    }
    // ponytail: дедуп слов на стыке не нужен — окно скользит ЗА committed, перекрытия нет по построению
    format!("{f} {t}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voc(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn clean_whisper_spaces_repeats_caps() {
        assert_eq!(clean_whisper("  привет   мир.  "), "Привет мир.");
        assert_eq!(clean_whisper("да да да"), "Да"); // схлопывание повторов + капитализация
        assert_eq!(clean_whisper("это это тест"), "Это тест");
    }

    #[test]
    fn clean_whisper_drops_hallucinations() {
        assert_eq!(clean_whisper("[музыка]"), "");
        assert_eq!(clean_whisper("Спасибо за просмотр"), "");
        assert_eq!(clean_whisper("Продолжение следует..."), "");
        assert_eq!(clean_whisper("Дякую за перегляд!"), "");
        // хвостовая титр-строка отрезается, осмысленное остаётся
        assert_eq!(clean_whisper("Открой проект. Спасибо за просмотр."), "Открой проект.");
    }

    #[test]
    fn apply_vocab_word_boundary_case_insensitive() {
        let v = voc(&[("клодбар", "ClaudeBar"), ("висперикс", "WhisperX")]);
        assert_eq!(apply_vocab("открой клодбар сейчас", &v), "открой ClaudeBar сейчас");
        assert_eq!(apply_vocab("Клодбар и висперикс", &v), "ClaudeBar и WhisperX");
        assert_eq!(apply_vocab("клодбарный код", &v), "клодбарный код"); // не по границе слова — не трогаем
        assert_eq!(apply_vocab("открой клодбар.", &v), "открой ClaudeBar."); // пунктуация сохранена
        assert_eq!(apply_vocab("без словаря", &[]), "без словаря");
    }

    #[test]
    fn process_pipeline() {
        let v = voc(&[("клодбар", "ClaudeBar")]);
        assert_eq!(process("  открой   клодбар.  ", &v, false), "Открой ClaudeBar.");
        assert_eq!(process("[музыка]", &v, false), "");
        assert_eq!(process("", &v, false), "");
    }

    #[test]
    fn process_trailing_space() {
        // ВКЛ: непустой результат получает хвостовой пробел (следующая вставка не липнет к точке)
        assert_eq!(process("раз.", &[], true), "Раз. ");
        assert_eq!(process("  привет мир  ", &[], true), "Привет мир ");
        // ВЫКЛ: без пробела
        assert_eq!(process("раз.", &[], false), "Раз.");
        // пустой результат -> пробел НЕ клеим (нечего вставлять)
        assert_eq!(process("[музыка]", &[], true), "");
        assert_eq!(process("", &[], true), "");
    }

    #[test]
    fn stable_prefix_commits_agreed_words() {
        // общий префикс «Первое предложение.» устоялся; хвост «Втор»/«Второе» ещё плавает
        assert_eq!(
            stable_prefix("Первое предложение. Втор", "Первое предложение. Второе и", 1),
            "Первое предложение."
        );
        // расхождение с первого слова -> ничего не фиксируем
        assert_eq!(stable_prefix("Абв где", "Ххх где", 0), "");
        // prev пустой -> нечего сравнивать
        assert_eq!(stable_prefix("", "Первое второе", 1), "");
        // margin съедает весь общий префикс -> ""
        assert_eq!(stable_prefix("раз два", "раз два", 3), "");
        // margin=0, полное совпадение -> весь текст
        assert_eq!(stable_prefix("раз два три", "раз два три", 0), "раз два три");
    }

    #[test]
    fn merge_committed_no_double_space() {
        assert_eq!(merge_committed("Первое.", "Второе."), "Первое. Второе.");
        // хвостовой пробел у fixed + ведущий у tail -> один пробел
        assert_eq!(merge_committed("Первое. ", "  Второе."), "Первое. Второе.");
        // пустой хвост -> fixed без изменений (даже с пробелом)
        assert_eq!(merge_committed("Раз. ", ""), "Раз. ");
        // пустой fixed -> хвост без ведущих пробелов
        assert_eq!(merge_committed("", "  Хвост"), "Хвост");
    }
}
