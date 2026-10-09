//! Whole-name glob matching: `*` for any run, `?` for one character.
//!
//! Matching runs on the same bytes the substring search uses (raw or folded),
//! so `--case-sensitive` governs globs too. `?` consumes one UTF-8 scalar
//! when the name is valid UTF-8 and one byte otherwise, so a non-ASCII
//! filename does not need one wildcard per byte.
//!
//! The matcher is the usual greedy two-pointer scan with backtracking to the
//! last `*`, which is O(n·m) in the worst case but linear in practice. When a
//! pattern contains a run of two or more literal bytes, that run must appear
//! verbatim in any matching name, so it doubles as a SIMD prefilter: see
//! [`longest_literal_run`]. A pattern with no such run (for example `*a*`)
//! falls back to matching every record.

/// Whether the pattern contains any wildcard.
#[inline]
pub fn has_wildcards(pattern: &[u8]) -> bool {
    pattern.iter().any(|&b| b == b'*' || b == b'?')
}

/// The longest run of literal bytes in the pattern.
///
/// Sound as a prefilter: a literal run has no wildcards, so it must appear
/// contiguously in any name the pattern matches.
pub fn longest_literal_run(pattern: &[u8]) -> &[u8] {
    let mut best = &pattern[..0];
    let mut start = 0usize;
    for i in 0..=pattern.len() {
        let literal = i < pattern.len() && pattern[i] != b'*' && pattern[i] != b'?';
        if !literal {
            if i - start > best.len() {
                best = &pattern[start..i];
            }
            start = i + 1;
        }
    }
    best
}

/// Length of the UTF-8 sequence at `i`, or 1 if the byte there is not a valid
/// sequence lead (which keeps matching defined for arbitrary bytes).
#[inline]
fn char_step(b: &[u8], i: usize) -> usize {
    let Some(&c) = b.get(i) else { return 0 };
    let n = match c {
        0x00..=0x7F => 1,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return 1,
    };
    if i + n > b.len() {
        return 1;
    }
    if n > 1 && !b[i + 1..i + n].iter().all(|&x| (0x80..=0xBF).contains(&x)) {
        return 1;
    }
    n
}

/// Whether `pattern` matches the whole of `text`.
pub fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let mut star_p = usize::MAX;
    let mut star_t = 0usize;
    while t < text.len() {
        match pattern.get(p) {
            Some(b'*') => {
                star_p = p;
                star_t = t;
                p += 1;
            }
            Some(b'?') => {
                p += 1;
                t += char_step(text, t).max(1);
            }
            Some(&c) if c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => {
                // Mismatch: let the last `*` swallow one more character.
                if star_p == usize::MAX || star_t >= text.len() {
                    return false;
                }
                star_t += char_step(text, star_t).max(1);
                p = star_p + 1;
                t = star_t;
            }
        }
    }
    while pattern.get(p) == Some(&b'*') {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pattern: &str, text: &str) -> bool {
        glob_match(pattern.as_bytes(), text.as_bytes())
    }

    #[test]
    fn no_wildcards_behaves_like_equality() {
        assert!(m("main.rs", "main.rs"));
        assert!(!m("main.rs", "main.rs2"));
        assert!(!m("main.rs", "xmain.rs"));
    }

    #[test]
    fn star_spans_any_run_including_empty() {
        assert!(m("*.rs", "main.rs"));
        assert!(m("*.rs", ".rs"));
        assert!(!m("*.rs", "main.rss"), "trailing text after the suffix");
        assert!(m("a*b*c", "abc"));
        assert!(m("a*b*c", "axxbyyc"));
        assert!(!m("a*b*c", "axxbyy"));
        assert!(m("*", ""));
        assert!(m("*", "anything"));
        assert!(m("**", "x"), "consecutive stars collapse");
        assert!(m("*.", "a."));
        assert!(!m("*.", "ab"));
    }

    #[test]
    fn question_matches_exactly_one_character() {
        assert!(m("a?c", "abc"));
        assert!(!m("a?c", "ac"));
        assert!(!m("a?c", "abcc"));
        assert!(m("??", "ab"));
        assert!(!m("??", "a"));
        assert!(!m("?", ""));
    }

    #[test]
    fn question_is_character_aware_for_utf8() {
        // 日 is three bytes; one `?` must still match it.
        assert!(m("?.txt", "日.txt"));
        assert!(m("??.txt", "日本.txt"));
        assert!(!m("?.txt", "日本.txt"));
        assert!(m("*日*", "x日本y"));
    }

    #[test]
    fn invalid_utf8_is_matched_bytewise() {
        let text = [0x41, 0xff, 0x42];
        assert!(glob_match(b"A?B", &text));
        assert!(glob_match(b"*", &text));
        assert!(glob_match(b"A*", &text));
    }

    #[test]
    fn longest_literal_run_picks_the_longest() {
        assert_eq!(longest_literal_run(b"*.rs"), b".rs");
        assert_eq!(longest_literal_run(b"src/*/gen_*.c"), b"/gen_");
        assert_eq!(longest_literal_run(b"src/*/gen_*.cpp"), b"/gen_");
        assert_eq!(longest_literal_run(b"a*bc*def"), b"def");
        assert_eq!(longest_literal_run(b"*a*"), b"a");
        assert_eq!(longest_literal_run(b"*?"), b"");
        assert_eq!(longest_literal_run(b"plain"), b"plain");
    }

    #[test]
    fn has_wildcards_detects_both() {
        assert!(has_wildcards(b"a*"));
        assert!(has_wildcards(b"a?"));
        assert!(!has_wildcards(b"abc"));
    }

    /// The reference is the obvious recursive definition, with no cleverness.
    fn reference(pattern: &[u8], text: &[u8]) -> bool {
        match pattern.first() {
            None => text.is_empty(),
            Some(b'*') => {
                (0..=text.len()).any(|k| reference(&pattern[1..], &text[k..]))
                    || reference(&pattern[1..], text)
            }
            Some(b'?') => {
                if text.is_empty() {
                    false
                } else {
                    reference(&pattern[1..], &text[char_step(text, 0).max(1)..])
                }
            }
            Some(&c) => !text.is_empty() && text[0] == c && reference(&pattern[1..], &text[1..]),
        }
    }

    #[test]
    fn matches_the_reference_implementation() {
        // Deterministic exhaustive sweep over a small alphabet, which covers
        // every interaction of star, question and literal at these lengths.
        let pat_alpha = *b"ab*?";
        let txt_alpha = *b"ab";
        for plen in 0..=4 {
            let mut pattern = vec![0u8; plen];
            let total = pat_alpha.len().pow(plen as u32);
            for mut n in 0..total {
                for slot in pattern.iter_mut() {
                    *slot = pat_alpha[n % pat_alpha.len()];
                    n /= pat_alpha.len();
                }
                for tlen in 0..=5 {
                    let mut text = vec![0u8; tlen];
                    let total_t = txt_alpha.len().pow(tlen as u32);
                    for mut k in 0..total_t {
                        for slot in text.iter_mut() {
                            *slot = txt_alpha[k % txt_alpha.len()];
                            k /= txt_alpha.len();
                        }
                        assert_eq!(
                            glob_match(&pattern, &text),
                            reference(&pattern, &text),
                            "pattern {:?} text {:?}",
                            String::from_utf8_lossy(&pattern),
                            String::from_utf8_lossy(&text)
                        );
                    }
                }
            }
        }
    }
}
