//! Short transfer codes like `7-cedar-river`.
//!
//! A code has two parts:
//! - the **nameplate** (`7`): a number the two peers find each other by.
//!   Either the sender picks it at random and announces it on the local
//!   network and the DHT, or a rendezvous server hands it out. It is not
//!   secret.
//! - the **words** (`cedar-river`): random words from the EFF short wordlist.
//!   They never leave the machines; the whole code is the SPAKE2 password.
//!
//! The wordlist is the EFF Short Wordlist #1 by the Electronic Frontier
//! Foundation (CC BY 3.0), with the one hyphenated word ("yo-yo") removed.
//! See `NOTICE`.

use std::fmt;
use std::sync::OnceLock;

/// Number of secret words in a generated code.
pub const CODE_WORDS: usize = 2;

/// Largest nameplate a code may contain.
pub const MAX_NAMEPLATE: u32 = 9_999_999;

/// Longest code string we try to parse.
const MAX_CODE_LEN: usize = 128;

const WORDLIST: &str = include_str!("wordlist.txt");

/// The wordlist, sorted and lowercase.
pub fn words() -> &'static [&'static str] {
    static WORDS: OnceLock<Vec<&'static str>> = OnceLock::new();
    WORDS.get_or_init(|| WORDLIST.lines().map(str::trim).collect())
}

/// Why a code could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodeError {
    /// Not of the form `<number>-<word>-<word>`.
    #[error("a code looks like 7-cedar-river: a number and {CODE_WORDS} words")]
    Format,
    /// The number part is not a valid nameplate.
    #[error("the code must start with a number between 1 and {MAX_NAMEPLATE}")]
    Nameplate,
    /// A word is not in the wordlist (likely a typo).
    #[error("\"{word}\" is not a code word{}", suggestion.map(|s| format!(" (did you mean \"{s}\"?)")).unwrap_or_default())]
    UnknownWord {
        /// The word as typed.
        word: String,
        /// The closest word in the list, if one is close.
        suggestion: Option<&'static str>,
    },
    /// The operating system's random number generator failed.
    #[error("could not generate random words")]
    Random,
}

/// A transfer code: a nameplate plus secret words.
///
/// `Debug` hides the words so codes cannot leak into logs by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct Code {
    nameplate: u32,
    words: Vec<&'static str>,
}

impl Code {
    /// A fresh code for `nameplate` with [`CODE_WORDS`] random words.
    pub fn generate(nameplate: u32) -> Result<Self, CodeError> {
        if !(1..=MAX_NAMEPLATE).contains(&nameplate) {
            return Err(CodeError::Nameplate);
        }
        let list = words();
        let words = (0..CODE_WORDS)
            .map(|_| random_below(list.len()).map(|i| list[i]))
            .collect::<Result<_, _>>()?;
        Ok(Self { nameplate, words })
    }

    /// A fresh code with a random nameplate, for finding peers without a
    /// server. The range is wide so unrelated senders rarely collide.
    pub fn random() -> Result<Self, CodeError> {
        let nameplate = random_below(MAX_NAMEPLATE as usize)? as u32 + 1;
        Self::generate(nameplate)
    }

    /// Reads a code typed by a person: case-insensitive, surrounding spaces
    /// ignored, and spaces accepted in place of hyphens.
    pub fn parse(input: &str) -> Result<Self, CodeError> {
        let input = input.trim();
        if input.is_empty() || input.len() > MAX_CODE_LEN {
            return Err(CodeError::Format);
        }
        let lower = input.to_ascii_lowercase();
        let mut parts = lower
            .split(|c: char| c == '-' || c.is_whitespace())
            .filter(|p| !p.is_empty());
        let nameplate = parts.next().ok_or(CodeError::Format)?;
        if nameplate.is_empty()
            || nameplate.len() > 7
            || !nameplate.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(CodeError::Nameplate);
        }
        let nameplate: u32 = nameplate.parse().map_err(|_| CodeError::Nameplate)?;
        if nameplate == 0 {
            return Err(CodeError::Nameplate);
        }
        let typed: Vec<&str> = parts.collect();
        if typed.len() != CODE_WORDS {
            return Err(CodeError::Format);
        }
        let list = words();
        let words = typed
            .into_iter()
            .map(|w| match list.binary_search(&w) {
                Ok(i) => Ok(list[i]),
                Err(_) => Err(CodeError::UnknownWord {
                    word: w.to_owned(),
                    suggestion: suggest(w),
                }),
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { nameplate, words })
    }

    /// The nameplate: the public number peers find each other by.
    pub fn nameplate(&self) -> u32 {
        self.nameplate
    }

    /// The SPAKE2 password: the canonical code string.
    pub fn password(&self) -> Vec<u8> {
        self.to_string().into_bytes()
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.nameplate)?;
        for w in &self.words {
            write!(f, "-{w}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Code({}-<redacted>)", self.nameplate)
    }
}

/// A uniformly random index below `n` from the OS generator.
fn random_below(n: usize) -> Result<usize, CodeError> {
    let n = u32::try_from(n).map_err(|_| CodeError::Random)?;
    // Rejection sampling avoids modulo bias.
    let zone = u32::MAX - (u32::MAX % n);
    loop {
        let x = getrandom::u32().map_err(|_| CodeError::Random)?;
        if x < zone {
            return Ok((x % n) as usize);
        }
    }
}

/// The wordlist entry closest to `typed`, if it is at most 2 edits away.
fn suggest(typed: &str) -> Option<&'static str> {
    if typed.len() > 16 {
        return None;
    }
    words()
        .iter()
        .map(|w| (edit_distance(typed, w), *w))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, w)| w)
}

/// Levenshtein distance over bytes (the wordlist is ASCII).
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wordlist_is_sorted_unique_lowercase() {
        let list = words();
        assert_eq!(list.len(), 1295);
        assert!(list.windows(2).all(|w| w[0] < w[1]));
        assert!(
            list.iter()
                .all(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase()))
        );
    }

    #[test]
    fn generated_codes_roundtrip() {
        for n in [1, 7, 42, MAX_NAMEPLATE] {
            let code = Code::generate(n).unwrap();
            assert_eq!(code.nameplate(), n);
            assert_eq!(Code::parse(&code.to_string()).unwrap(), code);
        }
    }

    #[test]
    fn random_codes_roundtrip() {
        for _ in 0..50 {
            let code = Code::random().unwrap();
            assert!((1..=MAX_NAMEPLATE).contains(&code.nameplate()));
            assert_eq!(Code::parse(&code.to_string()).unwrap(), code);
        }
    }

    #[test]
    fn generated_codes_vary() {
        let codes: std::collections::HashSet<String> = (0..50)
            .map(|_| Code::generate(1).unwrap().to_string())
            .collect();
        assert!(codes.len() > 40);
    }

    #[test]
    fn parsing_is_forgiving_about_case_and_separators() {
        let want = Code::parse("7-cedar-river").unwrap();
        for typed in [
            "7-cedar-river",
            " 7-Cedar-RIVER\n",
            "7 cedar river",
            "7--cedar - river",
        ] {
            assert_eq!(Code::parse(typed).unwrap(), want, "{typed:?}");
        }
        assert_eq!(want.to_string(), "7-cedar-river");
        assert_eq!(want.password(), b"7-cedar-river");
    }

    #[test]
    fn bad_codes_are_rejected() {
        use CodeError::*;
        let cases: &[(&str, CodeError)] = &[
            ("", Format),
            ("   ", Format),
            ("7", Format),
            ("7-cedar", Format),
            ("7-cedar-river-ocean", Format),
            ("cedar-river", Nameplate),
            ("0-cedar-river", Nameplate),
            ("+7-cedar-river", Nameplate),
            ("99999999-cedar-river", Nameplate),
            ("7x-cedar-river", Nameplate),
        ];
        for (typed, want) in cases {
            assert_eq!(&Code::parse(typed).unwrap_err(), want, "{typed:?}");
        }
        assert_eq!(Code::parse(&"7-".repeat(100)).unwrap_err(), Format);
        assert_eq!(Code::generate(0).unwrap_err(), Nameplate);
    }

    #[test]
    fn typos_get_a_suggestion() {
        let err = Code::parse("7-cedar-rivr").unwrap_err();
        assert_eq!(
            err,
            CodeError::UnknownWord {
                word: "rivr".into(),
                suggestion: Some("river")
            }
        );
        assert!(err.to_string().contains("did you mean \"river\""));
        let err = Code::parse("7-cedar-qqqqqqqq").unwrap_err();
        assert!(matches!(
            err,
            CodeError::UnknownWord {
                suggestion: None,
                ..
            }
        ));
    }

    #[test]
    fn debug_hides_the_words() {
        let code = Code::parse("7-cedar-river").unwrap();
        let shown = format!("{code:?}");
        assert!(
            !shown.contains("cedar") && !shown.contains("river"),
            "{shown}"
        );
    }

    #[test]
    fn edit_distance_basics() {
        assert_eq!(edit_distance("river", "river"), 0);
        assert_eq!(edit_distance("rivr", "river"), 1);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }
}
