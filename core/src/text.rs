//! Utilidades de texto que en Swift daba Foundation (`folding`, `localizedStandardCompare`…).

use std::cmp::Ordering;
use unicode_normalization::UnicodeNormalization;

/// Minúsculas y sin diacríticos: «Zoé» → «zoe».
pub fn fold(s: &str) -> String {
    s.nfd()
        .filter(|c| !is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase()
}

fn is_combining_mark(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

/// Palabras (letras y números) del texto, ya plegado.
pub fn words(s: &str) -> Vec<String> {
    fold(s)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Equivale a `localizedStandardContains`: sin mayúsculas ni diacríticos.
pub fn standard_contains(haystack: &str, needle: &str) -> bool {
    fold(haystack).contains(&fold(needle))
}

/// Equivale a `localizedStandardCompare`: sin mayúsculas ni diacríticos y con orden numérico
/// natural («Pista 2» < «Pista 10»).
pub fn standard_compare(a: &str, b: &str) -> Ordering {
    let (a, b) = (fold(a), fold(b));
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na = take_number(&mut ai);
                let nb = take_number(&mut bi);
                match na.cmp(&nb) {
                    Ordering::Equal => {}
                    o => return o,
                }
            }
            (Some(x), Some(y)) => {
                match x.cmp(&y) {
                    Ordering::Equal => {
                        ai.next();
                        bi.next();
                    }
                    o => return o,
                }
            }
        }
    }
}

/// Lee un número sin ceros a la izquierda; compara primero por longitud y luego por texto.
fn take_number(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> (usize, String) {
    let mut digits = String::new();
    while let Some(&c) = it.peek() {
        if c.is_ascii_digit() {
            digits.push(c);
            it.next();
        } else {
            break;
        }
    }
    let trimmed = digits.trim_start_matches('0').to_owned();
    (trimmed.len(), trimmed)
}

/// «183000 ms» → «3:03».
pub fn format_time(ms: i64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// «1234567» → «1,234,567».
pub fn format_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_removes_accents_and_case() {
        assert_eq!(fold("Zoé"), "zoe");
        assert_eq!(fold("ÁÉÍÓÚ ñ"), "aeiou n");
    }

    #[test]
    fn words_split_on_punctuation() {
        assert_eq!(words("The Weeknd — Blinding Lights!"), ["the", "weeknd", "blinding", "lights"]);
    }

    #[test]
    fn natural_compare() {
        assert_eq!(standard_compare("Pista 2", "Pista 10"), Ordering::Less);
        assert_eq!(standard_compare("alma", "Álvaro"), Ordering::Less);
        assert_eq!(standard_compare("Zoé", "zoe"), Ordering::Equal);
    }

    #[test]
    fn time_format() {
        assert_eq!(format_time(183_000), "3:03");
        assert_eq!(format_time(59_999), "0:59");
    }

    #[test]
    fn count_format_adds_thousands_separators() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(1_254_896_231), "1,254,896,231");
    }
}
