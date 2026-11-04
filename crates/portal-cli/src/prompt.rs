//! Typing a code at the prompt. Tab (or →) completes the word being typed
//! as soon as its letters fit only one code word, the few words it could
//! still become are shown while it is ambiguous, and a word that cannot be
//! a code word turns red as it is typed, before anyone is contacted.

use anyhow::bail;
use console::{Key, Term, style};
use portal_proto::code::{CODE_WORDS, words};

/// Longest line accepted; codes are far shorter.
const MAX_LEN: usize = 80;
/// While a word could still become this many code words or fewer, they
/// are listed after the cursor.
const MAX_HINTS: usize = 4;

/// Reads a code typed on `term` after `prompt`.
pub fn read_code(term: &Term, prompt: &str) -> anyhow::Result<String> {
    let mut line = String::new();
    loop {
        draw(term, prompt, &line, true)?;
        match term.read_key()? {
            Key::Enter => break,
            Key::Tab | Key::ArrowRight => line.push_str(&completion(&line)),
            Key::Backspace => {
                line.pop();
            }
            Key::CtrlC | Key::Escape => {
                term.write_line("")?;
                bail!("cancelled");
            }
            Key::Char(c) if is_code_char(c) && line.len() < MAX_LEN => line.push(c),
            _ => {}
        }
    }
    draw(term, prompt, &line, false)?;
    term.write_line("")?;
    Ok(line)
}

fn is_code_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == ' '
}

/// Redraws the prompt line: the typed text, then (when `hints`) the
/// completion or the candidates, dimmed, with the cursor after the text.
fn draw(term: &Term, prompt: &str, line: &str, hints: bool) -> std::io::Result<()> {
    let after = if hints { hint(line) } else { String::new() };
    term.clear_line()?;
    term.write_str(&format!("{prompt}{}{}", colored(line), style(&after).dim()))?;
    let back = after.chars().count();
    if back > 0 {
        term.move_cursor_left(back)?;
    }
    Ok(())
}

/// The parts of `line`: the number, then the words. Hyphens and spaces
/// both separate them, as [`portal_proto::Code::parse`] accepts.
fn parts(line: &str) -> Vec<&str> {
    line.split(['-', ' ']).collect()
}

/// The code words starting with `prefix` (lowercase).
fn candidates(prefix: &str) -> &'static [&'static str] {
    let list = words();
    let start = list.partition_point(|w| *w < prefix);
    let end = start + list[start..].partition_point(|w| w.starts_with(prefix));
    &list[start..end]
}

/// What Tab adds to `line`: the rest of the word being typed once its
/// letters fit only one code word (as much as all candidates share
/// otherwise), and a hyphen when another word follows.
pub fn completion(line: &str) -> String {
    let parts = parts(line);
    let Some(current) = parts.last().filter(|p| !p.is_empty()) else {
        return String::new();
    };
    let more_follow = parts.len() <= CODE_WORDS;
    if parts.len() == 1 {
        let number = current.bytes().all(|b| b.is_ascii_digit());
        return if number { "-".into() } else { String::new() };
    }
    let lower = current.to_ascii_lowercase();
    let found = candidates(&lower);
    let (Some(first), Some(last)) = (found.first(), found.last()) else {
        return String::new();
    };
    // The sorted list's first and last candidates share the longest prefix.
    let shared = first
        .bytes()
        .zip(last.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let mut rest = first[lower.len()..shared].to_owned();
    if found.len() == 1 && more_follow {
        rest.push('-');
    }
    rest
}

/// What is shown dimmed after the cursor: the completion without its
/// hyphen, or the candidates while there are a few.
fn hint(line: &str) -> String {
    let rest = completion(line);
    let rest = rest.trim_end_matches('-');
    if !rest.is_empty() {
        return rest.to_owned();
    }
    let parts = parts(line);
    match parts.last() {
        Some(current) if parts.len() > 1 && !current.is_empty() => {
            let found = candidates(&current.to_ascii_lowercase());
            if (2..=MAX_HINTS).contains(&found.len()) {
                format!("  ({})", found.join(", "))
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

/// `line` with each part that cannot be part of a code in red: a number
/// that is not one, a word no code word starts with, or a word too many.
fn colored(line: &str) -> String {
    let mut out = String::new();
    let mut index = 0;
    let mut word = String::new();
    let flush = |word: &mut String, index: &mut usize, out: &mut String| {
        if word.is_empty() {
            return;
        }
        let fine = match *index {
            0 => word.bytes().all(|b| b.is_ascii_digit()),
            i if i <= CODE_WORDS => !candidates(&word.to_ascii_lowercase()).is_empty(),
            _ => false,
        };
        if fine {
            out.push_str(word);
        } else {
            out.push_str(&style(&*word).red().to_string());
        }
        word.clear();
        *index += 1;
    };
    for c in line.chars() {
        if c == '-' || c == ' ' {
            flush(&mut word, &mut index, &mut out);
            out.push(c);
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut index, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_completes_words_once_they_are_unambiguous() {
        assert_eq!(completion("7"), "-");
        assert_eq!(completion("7-"), "");
        // "ced" fits only "cedar"; a hyphen follows since a word is missing.
        assert_eq!(completion("7-ced"), "ar-");
        assert_eq!(completion("7-CED"), "ar-");
        assert_eq!(completion("7-cedar"), "-");
        // The last word gets no hyphen.
        assert_eq!(completion("7-cedar-rive"), "r");
        assert_eq!(completion("7 cedar rive"), "r");
        // "riv" could be "rival" or "river": nothing to add, both are shown.
        assert_eq!(completion("7-cedar-riv"), "");
        assert_eq!(hint("7-cedar-riv"), "  (rival, river)");
        // Not a code word: nothing to add.
        assert_eq!(completion("7-qqq"), "");
    }

    #[test]
    fn ambiguous_words_complete_as_far_as_they_agree() {
        let found = candidates("ar");
        assert!(found.len() > 1);
        let added = completion("7-ar");
        assert!(found.iter().all(|w| w.starts_with(&format!("ar{added}"))));
        assert!(!added.ends_with('-'));
    }

    #[test]
    fn every_word_completes_from_its_first_five_letters() {
        for word in words() {
            let prefix = &word[..word.len().min(5)];
            let line = format!("7-{prefix}");
            let completed = format!("{line}{}", completion(&line));
            assert!(completed.starts_with(&format!("7-{word}")), "{word}");
        }
    }

    #[test]
    fn a_few_candidates_are_shown() {
        assert_eq!(hint("7-ced"), "ar");
        let two = words()
            .iter()
            .map(|w| &w[..3])
            .find(|p| candidates(p).len() == 2 && completion(&format!("7-{p}")).is_empty())
            .expect("some prefix fits two words");
        let shown = hint(&format!("7-{two}"));
        for w in candidates(two) {
            assert!(shown.contains(w), "{shown}");
        }
    }

    #[test]
    fn mistakes_are_red() {
        console::set_colors_enabled(true);
        let plain = colored("7-cedar-riv");
        assert_eq!(console::strip_ansi_codes(&plain), "7-cedar-riv");
        assert_eq!(plain, "7-cedar-riv", "nothing to mark");
        assert_ne!(colored("7-cedr"), "7-cedr");
        assert_ne!(colored("x7-cedar"), "x7-cedar");
        assert_ne!(colored("7-cedar-river-oak"), "7-cedar-river-oak");
    }
}
