//! Human-friendly formatting helpers with no external dependencies.

/// `1536` → `"1.5 KiB"`. Bytes below 1 KiB print as a plain count.
pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Parse `10K`, `4M`, `1G`, `512`, `2MiB`, `3gb` into bytes (binary units).
pub fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let digits_end = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(digits_end);
    let num: f64 = num.parse().map_err(|_| format!("invalid size `{s}`"))?;
    let unit = unit.trim().to_ascii_lowercase();
    let unit = unit.trim_end_matches("ib").trim_end_matches('b');
    let mult: f64 = match unit {
        "" => 1.0,
        "k" => 1024.0,
        "m" => 1024.0_f64.powi(2),
        "g" => 1024.0_f64.powi(3),
        "t" => 1024.0_f64.powi(4),
        "p" => 1024.0_f64.powi(5),
        _ => return Err(format!("unknown size unit in `{s}`")),
    };
    Ok((num * mult) as u64)
}

/// Nanoseconds since the Unix epoch → `YYYY-MM-DD HH:MM` in UTC.
pub fn format_utc(ns: i64) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60
    )
}

/// A point in time for `--after` / `--before`:
/// `2024-02-29`, `2024-02-29T13:05`, `2024-02-29T13:05:09Z` (all UTC), or an
/// offset back from now: `-45s`, `-30m`, `-2h`, `-7d`, `-3w`.
pub fn parse_time(s: &str, now_ns: i64) -> Result<i64, String> {
    let s = s.trim();
    if let Some(rel) = s.strip_prefix('-') {
        let digits_end = rel.find(|c: char| !c.is_ascii_digit()).unwrap_or(rel.len());
        let (num, unit) = rel.split_at(digits_end);
        let n: i64 = num
            .parse()
            .map_err(|_| format!("invalid relative time `{s}`"))?;
        let secs: i64 = match unit.trim() {
            "s" => 1,
            "m" => 60,
            "h" => 3_600,
            "d" => 86_400,
            "w" => 604_800,
            _ => return Err(format!("unknown unit in `{s}` (use s, m, h, d, w)")),
        };
        return Ok(now_ns.saturating_sub(n.saturating_mul(secs).saturating_mul(1_000_000_000)));
    }
    let (date, time) = match s.split_once('T') {
        Some((d, t)) => (d, Some(t.trim_end_matches('Z'))),
        None => (s, None),
    };
    let mut parts = date.split('-');
    let bad = || format!("invalid date `{s}` (expected YYYY-MM-DD[THH:MM[:SS]][Z] or -7d)");
    let y: i64 = parts.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?;
    let m: u32 = parts.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?;
    let d: u32 = parts.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(bad());
    }
    let mut secs = 0i64;
    if let Some(t) = time {
        let mut tp = t.split(':');
        let h: i64 = tp.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?;
        let mi: i64 = tp.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?;
        let sec: i64 = match tp.next() {
            Some(x) => x.parse().map_err(|_| bad())?,
            None => 0,
        };
        if tp.next().is_some()
            || !(0..24).contains(&h)
            || !(0..60).contains(&mi)
            || !(0..61).contains(&sec)
        {
            return Err(bad());
        }
        secs = h * 3600 + mi * 60 + sec;
    }
    let days = days_from_civil(y, m, d);
    Ok((days * 86_400 + secs).saturating_mul(1_000_000_000))
}

/// Howard Hinnant's days-from-civil; the inverse of `civil_from_days`.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from(if m > 2 { m - 3 } else { m + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Howard Hinnant's days-from-civil inverse; valid for the full i64 day range
/// a filesystem will ever produce.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(human_size(250 * 1024 * 1024), "250 MiB");
    }

    #[test]
    fn parse_sizes() {
        assert_eq!(parse_size("512").unwrap(), 512);
        assert_eq!(parse_size("10K").unwrap(), 10 * 1024);
        assert_eq!(parse_size("4m").unwrap(), 4 * 1024 * 1024);
        assert_eq!(parse_size("1GiB").unwrap(), 1 << 30);
        assert_eq!(parse_size("1.5k").unwrap(), 1536);
        assert!(parse_size("abc").is_err());
        assert!(parse_size("3x").is_err());
    }

    #[test]
    fn parse_times() {
        const NS: i64 = 1_000_000_000;
        assert_eq!(parse_time("1970-01-01", 0).unwrap(), 0);
        assert_eq!(
            parse_time("2024-02-29T12:34:56Z", 0).unwrap(),
            1_709_210_096 * NS
        );
        assert_eq!(
            parse_time("2024-02-29T12:34", 0).unwrap(),
            (1_709_210_096 - 56) * NS
        );
        assert_eq!(parse_time("-1d", 86_400 * NS).unwrap(), 0);
        assert_eq!(parse_time("-90m", 2 * 3_600 * NS).unwrap(), 1_800 * NS);
        assert_eq!(parse_time("-2w", 0).unwrap(), -14 * 86_400 * NS);
        for bad in [
            "yesterday",
            "2024-13-01",
            "2024-02-29T25:00",
            "-3x",
            "2024-02",
        ] {
            assert!(parse_time(bad, 0).is_err(), "{bad}");
        }
        // round trip with the formatter
        let t = parse_time("1999-12-31T23:59:00Z", 0).unwrap();
        assert_eq!(format_utc(t), "1999-12-31 23:59");
    }

    #[test]
    fn dates() {
        assert_eq!(format_utc(0), "1970-01-01 00:00");
        // 2024-02-29T12:34:56Z
        assert_eq!(
            format_utc(1_709_210_096 * 1_000_000_000),
            "2024-02-29 12:34"
        );
        assert_eq!(format_utc(-1_000_000_000), "1969-12-31 23:59");
    }
}
