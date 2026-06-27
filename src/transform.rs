// FILE: src/transform.rs
// VERSION: 1.0.0
// START_MODULE_CONTRACT
//   PURPOSE: Пост-обработка распознанного текста перед вставкой: чистка типового мусора Whisper + кастомный словарь пост-заменой.
//   SCOPE: clean_whisper (теги в скобках, повторы слов, галлюцинации-предложения, капитализация), apply_vocab (замена по словарю по границе слова, регистронезависимо), process (связка). Всё чистое.
//   DEPENDS: none
//   LINKS: M-TRANSFORM
//   ROLE: RUNTIME
//   MAP_MODE: EXPORTS
// END_MODULE_CONTRACT
//
// START_MODULE_MAP
//   clean_whisper - чистое: убрать [теги], схлопнуть повторы слов, выкинуть предложения-галлюцинации, капитализировать
//   apply_vocab   - чистое: замена слов по словарю wrong->right (регистронезависимо, по границе слова)
//   process       - чистое: clean_whisper -> apply_vocab
// END_MODULE_MAP
//
// START_CHANGE_SUMMARY
//   LAST_CHANGE: v1.0.0 - Phase-18 step-4: чистка вывода Whisper (список галлюцинаций на тишине/шуме,
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
//   PURPOSE: Полная пост-обработка перед вставкой: чистка мусора, затем словарь.
//   INPUTS: { text: &str; vocab: &[(String, String)] }
//   OUTPUTS: { String - готовый к вставке текст }
//   SIDE_EFFECTS: none
// END_CONTRACT: process
pub fn process(text: &str, vocab: &[(String, String)]) -> String {
    apply_vocab(&clean_whisper(text), vocab)
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
        assert_eq!(process("  открой   клодбар.  ", &v), "Открой ClaudeBar.");
        assert_eq!(process("[музыка]", &v), "");
        assert_eq!(process("", &v), "");
    }
}
