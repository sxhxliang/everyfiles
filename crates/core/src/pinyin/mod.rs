//! Pinyin matching for Han-character names (feature `pinyin`).
//!
//! A pinyin pattern is typed in ASCII. Every letter of the needle either
//! matches an ASCII character of the name or part of a Han character's
//! reading: a Han character is consumed by the initial of one of its readings
//! (`p` for 拼 `pin`) or by a prefix of one (`so` and `sou` both consume 搜
//! `sou`). Readings are the tone-less ones from the [`pinyin`] crate's data
//! (41k characters, polyphones included), so `nv` and `nu` both match 女, and
//! both `chongqing` and `zhongqing` match 重庆.
//!
//! Matching is **contiguous**, exactly as a substring match is: the needle has
//! to be consumed by a run of adjacent characters of the name, each taking at
//! least one letter, and the run may start anywhere. So `pysousuoeve` matches
//! `拼音搜索Everything` — `p` 拼, `y` 音, `sou` 搜, `suo` 索, then `e` `v` `e`
//! of `Everything` — and `sousuo` matches `我的搜索工具` starting mid-name.
//! Letters are never skipped, so `pyev` does not match `拼音搜索Everything`.
//!
//! A character with no reading (an emoji, kana, a Cyrillic letter) is opaque:
//! only the same character in the needle consumes it. Invalid UTF-8 bytes are
//! skipped instead of becoming replacement characters, so they can never break
//! a match that would otherwise hold.
//!
//! [`matches`] is the whole predicate. The index path ([`Matcher`] driven by
//! [`Query::pinyin`](crate::query::Query)) allocates nothing per record and
//! answers names with no non-ASCII byte from the ordinary SIMD substring scan,
//! running the DP only on names that can hold a Han character.

use crate::fold;
use pinyin::ToPinyinMulti;

/// Longest reading in the data, in ASCII bytes (`zhuang`, `chuang`). The
/// `every_reading_fits` test walks the whole table and holds this true.
const MAX_READING: usize = 8;

/// Needle length the bit-parallel path can hold: one reachable position per
/// needle byte, plus the accepting position, in a `u128`. Longer needles take
/// [`Matcher::is_match_long`].
const MAX_BITS: usize = 127;

/// A compiled pinyin pattern.
///
/// ```
/// # #[cfg(feature = "pinyin")] {
/// use everyfiles_core::pinyin::Matcher;
/// let m = Matcher::new(b"pysousuoeve");
/// assert!(m.is_match("拼音搜索everything".as_bytes()));
/// assert!(!m.is_match(b"python"));
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct Matcher {
    needle: Vec<u8>,
    /// `mask[b]` has bit `i` set iff `needle[i] == b`.
    mask: Box<[u128; 256]>,
    /// `1 << needle.len()`, or 0 when the needle is too long for the masks.
    accept: u128,
}

impl Matcher {
    /// Fold `needle` and precompile it. An empty needle matches everything.
    pub fn new(needle: &[u8]) -> Self {
        let needle = fold::fold(needle);
        let mut mask = Box::new([0u128; 256]);
        let mut accept = 0;
        if needle.len() <= MAX_BITS {
            for (i, &b) in needle.iter().enumerate() {
                mask[b as usize] |= 1u128 << i;
            }
            accept = 1u128 << needle.len();
        }
        Self {
            needle,
            mask,
            accept,
        }
    }

    /// The folded needle.
    pub fn needle(&self) -> &[u8] {
        &self.needle
    }

    /// `true` if the pattern is contained in `folded_name`.
    ///
    /// `folded_name` must already be case-folded ([`crate::fold`]); this is
    /// what keeps the hot path allocation-free, and
    /// [`Index::folded`](crate::index::Index::folded) is exactly that. Use
    /// [`matches`] to fold for you.
    pub fn is_match(&self, folded_name: &[u8]) -> bool {
        if self.needle.is_empty() {
            return true;
        }
        if self.needle.len() > MAX_BITS {
            return self.is_match_long(folded_name, false);
        }
        let mut reach: u128 = 1;
        for c in Units::new(folded_name) {
            // Bit 0 is the "start here" state: the run may begin at any
            // character, and no character may be skipped, so nothing else
            // carries over from the previous one.
            let mut next: u128 = 1;
            for_each_reading(c, |alt, prefix| {
                next |= advance(reach, &self.mask, alt, prefix);
            });
            reach = next;
            if reach & self.accept != 0 {
                return true;
            }
        }
        false
    }

    /// `true` if the pattern consumes every character of `folded_name`.
    pub fn is_match_exact(&self, folded_name: &[u8]) -> bool {
        if self.needle.len() > MAX_BITS {
            return self.is_match_long(folded_name, true);
        }
        let mut reach: u128 = 1;
        for c in Units::new(folded_name) {
            let mut next: u128 = 0;
            for_each_reading(c, |alt, prefix| {
                next |= advance(reach, &self.mask, alt, prefix);
            });
            if next == 0 {
                return false;
            }
            reach = next;
        }
        reach & self.accept != 0
    }

    /// The positional fallback for needles past [`MAX_BITS`]: same recurrence,
    /// one reachable position per bit.
    fn is_match_long(&self, folded_name: &[u8], exact: bool) -> bool {
        let m = self.needle.len();
        let mut reach = vec![false; m + 1];
        reach[0] = true;
        for c in Units::new(folded_name) {
            let mut next = vec![false; m + 1];
            next[0] = !exact;
            for_each_reading(c, |alt, prefix| {
                let lo = if prefix { 1 } else { alt.len() };
                for (p, &live) in reach.iter().enumerate() {
                    if !live || p + lo > m {
                        continue;
                    }
                    for l in lo..=alt.len() {
                        if p + l <= m && self.needle[p..p + l] == alt[..l] {
                            next[p + l] = true;
                        }
                    }
                }
            });
            reach = next;
            if !exact && reach[m] {
                return true;
            }
        }
        exact && reach[m]
    }
}

/// Positions reachable by letting one character consume a prefix of `alt`.
///
/// `reach` is the set of needle positions consumed before this character;
/// bit `p + l` is set when `needle[p..p + l] == alt[..l]` for a reachable `p`.
/// `prefix` allows every `l` up to `alt.len()` (a reading); otherwise only the
/// whole `alt` counts (an ASCII character, or a Han character matched as
/// itself).
#[inline]
fn advance(reach: u128, mask: &[u128; 256], alt: &[u8], prefix: bool) -> u128 {
    let mut x = reach;
    let mut out = 0u128;
    let last = alt.len();
    for (k, &b) in alt.iter().enumerate() {
        x &= mask[b as usize] >> k;
        if x == 0 {
            return out;
        }
        if prefix || k + 1 == last {
            out |= x << (k + 1);
        }
    }
    out
}

/// Call `f(bytes, may_consume_a_prefix)` once per way the pattern may consume
/// `c`: each of its readings (prefix-matchable), and the character itself
/// (whole, so a Han character pasted into the needle still matches).
fn for_each_reading(c: char, mut f: impl FnMut(&[u8], bool)) {
    let mut lit = [0u8; 4];
    if c.is_ascii() {
        lit[0] = c as u8;
        f(&lit[..1], false);
        return;
    }
    if let Some(readings) = c.to_pinyin_multi() {
        let mut buf = [0u8; MAX_READING];
        for p in readings {
            let mut u_at = None;
            let n = write_reading(p.plain(), &mut buf, &mut u_at);
            if n == 0 {
                continue;
            }
            f(&buf[..n], true);
            // `ü` is typed `v` by every common input method, but `u` is what
            // someone who cannot type it will reach for; take both.
            if let Some(i) = u_at {
                let mut alt = buf;
                alt[i] = b'u';
                f(&alt[..n], true);
            }
        }
    }
    let lit = c.encode_utf8(&mut lit);
    f(lit.as_bytes(), false);
}

/// Copy one reading into `buf` as ASCII, remembering where `ü` became `v`.
/// Returns 0 if the reading does not fit, which [`MAX_READING`] rules out.
fn write_reading(plain: &str, buf: &mut [u8; MAX_READING], u_at: &mut Option<usize>) -> usize {
    let mut n = 0;
    for ch in plain.chars() {
        let b = match ch {
            'ü' => {
                *u_at = Some(n);
                b'v'
            }
            'ê' => b'e',
            c if c.is_ascii() => c as u8,
            _ => continue,
        };
        if n == buf.len() {
            debug_assert!(false, "reading {plain:?} does not fit in MAX_READING");
            return 0;
        }
        buf[n] = b;
        n += 1;
    }
    n
}

/// The characters of a folded name, in order.
///
/// Invalid UTF-8 is skipped one byte at a time rather than decoded lossily:
/// a replacement character would be an opaque unit in the middle of a name and
/// would stop matches that should hold.
struct Units<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Units<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
}

impl Iterator for Units<'_> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            let n = utf8_len(b);
            if n == 1 {
                self.pos += 1;
                return Some(b as char);
            }
            if n > 1
                && self.pos + n <= self.bytes.len()
                && let Ok(s) = std::str::from_utf8(&self.bytes[self.pos..self.pos + n])
            {
                self.pos += n;
                return s.chars().next();
            }
            self.pos += 1;
        }
        None
    }
}

/// Bytes in the UTF-8 sequence starting with `b`, or 0 if `b` cannot start one.
#[inline]
fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => 0,
    }
}

/// `true` if `haystack` matches the pinyin pattern `needle`.
///
/// ```
/// # #[cfg(feature = "pinyin")] {
/// use everyfiles_core::pinyin;
/// assert!(pinyin::matches("pysousuoeve", "拼音搜索Everything"));
/// assert!(pinyin::matches("sousuo", "我的搜索工具"));
/// assert!(!pinyin::matches("pyev", "拼音搜索Everything"));
/// # }
/// ```
pub fn matches(needle: &str, haystack: &str) -> bool {
    matches_bytes(needle.as_bytes(), haystack.as_bytes())
}

/// [`matches`] over raw bytes; both sides are case-folded first.
pub fn matches_bytes(needle: &[u8], haystack: &[u8]) -> bool {
    Matcher::new(needle).is_match(&fold::fold(haystack))
}

/// The name this crate's Chinese documentation uses for [`matches`].
pub fn 拼音_匹配(needle: &str, haystack: &str) -> bool {
    matches(needle, haystack)
}

pub(crate) mod search;
pub(crate) use search::search;

#[cfg(test)]
mod tests {
    use super::*;

    fn folded(s: &str) -> Vec<u8> {
        fold::fold(s.as_bytes())
    }

    #[test]
    fn the_documented_example() {
        assert!(matches("pysousuoeve", "拼音搜索Everything"));
        assert!(拼音_匹配("pysousuoeve", "拼音搜索Everything"));
        // The needle must be consumed without skipping a character.
        assert!(!matches("pyev", "拼音搜索Everything"));
        assert!(!matches("pysuoeve", "拼音搜索Everything"));
    }

    #[test]
    fn initials() {
        assert!(matches("py", "拼音"));
        assert!(matches("pysousuo", "拼音搜索"));
        assert!(matches("bg", "报告"));
        assert!(matches("wdssgj", "我的搜索工具"));
        assert!(!matches("pz", "拼音"));
    }

    #[test]
    fn whole_readings() {
        assert!(matches("pinyin", "拼音"));
        assert!(matches("sousuo", "搜索"));
        assert!(matches("pinyinsousuo", "拼音搜索"));
        // A reading is consumed whole or by a prefix of itself, never in part.
        assert!(matches("so", "搜索"));
        assert!(matches("sous", "搜索"));
        // `sou` is not a prefix of 索's reading either, so `suo` has to be
        // the run 索 alone; it still matches, starting there.
        assert!(matches("suo", "搜索"));
    }

    #[test]
    fn the_run_may_start_anywhere_but_may_not_skip() {
        assert!(matches("sousuo", "我的搜索工具"));
        assert!(matches("gj", "我的搜索工具"));
        // A character between two consumed ones has to consume something too.
        assert!(!matches("sousuo", "搜-索"));
        assert!(!matches("ss", "搜-索")); // the `-` needs a letter too
    }

    #[test]
    fn ascii_names_match_like_a_substring() {
        for (needle, name, want) in [
            ("ab", "a-b", false),
            ("ab", "ab", true),
            ("ab", "aab", true),
            ("ba", "ab", false),
            ("a", "ab", true),
            ("ab", "ab cd", true),
            ("abc", "ab", false),
            ("b c", "ab cd", true),
        ] {
            assert_eq!(matches(needle, name), want, "{needle:?} in {name:?}");
            assert_eq!(matches(needle, name), name.contains(needle));
        }
    }

    #[test]
    fn case_is_folded_away() {
        assert!(matches("EVE", "拼音搜索Everything"));
        assert!(matches("py", "拼音搜索EVERYTHING"));
        assert!(matches("eve", "拼音搜索Everything"));
    }

    #[test]
    fn a_han_character_in_the_needle_matches_itself() {
        assert!(matches("搜索", "我的搜索工具"));
        assert!(matches("拼音搜索", "拼音搜索Everything"));
        assert!(!matches("搜索", "我的工具"));
    }

    #[test]
    fn polyphones_take_every_reading() {
        assert!(matches("chongqing", "重庆"));
        assert!(matches("zhongqing", "重庆"));
        assert!(matches("cq", "重庆"));
        assert!(matches("yinhang", "银行"));
        assert!(matches("xing", "银行"));
        assert!(matches("hang", "银行"));
        assert!(!matches("cq", "上海"));
    }

    #[test]
    fn u_umlaut_is_v_or_u() {
        assert!(matches("nv", "女装"));
        assert!(matches("nu", "女装"));
        assert!(matches("nz", "女装"));
        assert!(matches("lv", "绿"));
        assert!(matches("lu", "绿"));
        assert!(matches("lve", "略"));
        assert!(matches("lue", "略"));
    }

    #[test]
    fn characters_without_a_reading_are_opaque() {
        assert!(!matches("ab", "a b")); // the space needs a letter too
        assert!(matches("ab", "ab"));
        // An emoji is a character like any other: only itself consumes it.
        assert!(!matches("ab", "a📄b"));
        assert!(matches("a📄b", "a📄b"));
        assert!(matches("bg", "报告📄"));
        assert!(!matches("bg", "报📄告"));
        // Kana and Cyrillic likewise.
        assert!(!matches("ry", "日の本"));
        assert!(matches("日", "日"));
    }

    #[test]
    fn invalid_utf8_is_skipped_not_replaced() {
        let m = Matcher::new(b"ab");
        assert!(m.is_match(&folded_bytes(b"a\xffb")));
        assert!(m.is_match(&folded_bytes(b"\xffab")));
        assert!(m.is_match(&folded_bytes(b"a\xe4\xb8b"))); // truncated 3-byte char
        assert!(!m.is_match(&folded_bytes(b"a\xffc")));
    }

    fn folded_bytes(src: &[u8]) -> Vec<u8> {
        fold::fold(src)
    }

    #[test]
    fn empty_needle_matches_everything() {
        assert!(matches("", ""));
        assert!(matches("", "拼音搜索Everything"));
    }

    #[test]
    fn exact_consumes_the_whole_name() {
        let m = Matcher::new(b"pysousuoeve");
        let name = folded("拼音搜索Everything");
        assert!(m.is_match(&name));
        assert!(!m.is_match_exact(&name));

        let full = Matcher::new(b"pysousuoeverything");
        assert!(full.is_match_exact(&name));
        assert!(full.is_match(&name));

        // Whole-name means the whole name, not a prefix of it.
        let short = Matcher::new(b"pysousuoeverythin");
        assert!(!short.is_match_exact(&name));
    }

    #[test]
    fn exact_on_ascii_is_equality() {
        assert!(Matcher::new(b"ab").is_match_exact(b"ab"));
        assert!(!Matcher::new(b"ab").is_match_exact(b"abc"));
        assert!(!Matcher::new(b"ab").is_match_exact(b"a-b"));
        assert!(!Matcher::new(b"ab").is_match_exact(b"aab"));
    }

    #[test]
    fn needles_past_the_bit_parallel_limit_agree() {
        // 50 × 搜 then 索: the needle is 153 bytes, past MAX_BITS, so both
        // sides of this test run the positional fallback.
        let name = "搜".repeat(50) + "索";
        let full = "sou".repeat(50) + "suo";
        let m = Matcher::new(full.as_bytes());
        assert!(m.needle().len() > MAX_BITS);
        assert!(m.is_match(&folded(&name)));
        assert!(m.is_match_exact(&folded(&name)));

        // One character more in the name than the needle can cover.
        let shorter = Matcher::new("sou".repeat(50).as_bytes());
        assert!(shorter.is_match(&folded(&name)));
        assert!(!shorter.is_match_exact(&folded(&name)));

        // A needle the name cannot hold at all.
        let miss = Matcher::new("z".repeat(150).as_bytes());
        assert!(!miss.is_match(&folded(&name)));
        assert!(!miss.is_match_exact(&folded(&name)));
    }

    /// The definition, transcribed as backtracking: try every character as the
    /// start of the run, let each character consume a prefix of one reading,
    /// and accept when the needle is used up (or, for `exact`, when the name is
    /// too). Independent of both the bit-parallel and the positional paths.
    fn consume(needle: &[u8], at: usize, units: &[char], i: usize, exact: bool) -> bool {
        if at == needle.len() {
            return !exact || i == units.len();
        }
        if i == units.len() {
            return false;
        }
        let mut ok = false;
        for_each_reading(units[i], |alt, prefix| {
            if ok {
                return;
            }
            let lo = if prefix { 1 } else { alt.len() };
            for l in lo..=alt.len() {
                if at + l <= needle.len()
                    && needle[at..at + l] == alt[..l]
                    && consume(needle, at + l, units, i + 1, exact)
                {
                    ok = true;
                    return;
                }
            }
        });
        ok
    }

    fn reference(needle: &[u8], name: &[u8], exact: bool) -> bool {
        let units: Vec<char> = Units::new(name).collect();
        if exact {
            return consume(needle, 0, &units, 0, true);
        }
        (0..units.len()).any(|start| consume(needle, 0, &units, start, false))
    }

    /// Exhaustive comparison against the reference on a small alphabet, for
    /// every name of up to three characters and every needle of up to three
    /// letters — both modes, and both matcher implementations.
    #[test]
    fn agrees_with_the_reference_on_a_small_alphabet() {
        const ALPHABET: [&str; 7] = ["搜", "索", "的", "a", "b", "-", "の"];
        // `搜`/`索` in a needle are whole multi-byte alternatives, which is
        // the case that a per-byte implementation gets wrong.
        const LETTERS: [&str; 10] = ["a", "b", "s", "o", "u", "d", "e", "-", "搜", "索"];

        let mut names: Vec<Vec<u8>> = vec![Vec::new()];
        let mut level = vec![String::new()];
        for _ in 0..3 {
            let mut next = Vec::new();
            for n in &level {
                for c in ALPHABET {
                    next.push(format!("{n}{c}"));
                }
            }
            names.extend(next.iter().map(|n| folded(n)));
            level = next;
        }

        let mut needles: Vec<Vec<u8>> = Vec::new();
        let mut level = vec![String::new()];
        for _ in 0..3 {
            let mut next = Vec::new();
            for n in &level {
                for c in LETTERS {
                    next.push(format!("{n}{c}"));
                }
            }
            needles.extend(next.iter().map(|n| n.as_bytes().to_vec()));
            level = next;
        }

        let mut checked = 0u32;
        for n in &needles {
            let m = Matcher::new(n);
            for name in &names {
                for exact in [false, true] {
                    let got = if exact {
                        m.is_match_exact(name)
                    } else {
                        m.is_match(name)
                    };
                    assert_eq!(
                        got,
                        reference(n, name, exact),
                        "{n:?} vs {name:?} exact={exact}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 400_000, "only {checked} pairs checked");
    }

    /// Every reading in the table must fit `MAX_READING`, or it would be
    /// silently dropped from matching.
    #[test]
    fn every_reading_fits() {
        let mut checked = 0u32;
        for cp in 0..=0x2FFFF {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            let Some(readings) = c.to_pinyin_multi() else {
                continue;
            };
            let mut buf = [0u8; MAX_READING];
            for p in readings {
                let mut u_at = None;
                let n = write_reading(p.plain(), &mut buf, &mut u_at);
                assert!(n > 0, "U+{cp:04X} reading {:?} did not fit", p.plain());
                assert!(buf[..n].is_ascii());
                checked += 1;
            }
        }
        assert!(checked > 40_000, "only {checked} readings seen");
    }
}
