//! Docker timestamps. Log lines carry RFC 3339 times with nanoseconds, and
//! the engine reads `since` as Unix seconds with a fraction, so a follow-up
//! request resumes one nanosecond after the last line it returned.

/// Seconds and nanoseconds since 1970 of an RFC 3339 time such as
/// `2024-05-01T12:34:56.123456789Z` or `...+02:00`.
pub fn parse_rfc3339(text: &str) -> Option<(i64, u32)> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || !matches!(bytes[10], b'T' | b't' | b' ') {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let digits = text.get(range)?;
        digits
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| digits.parse().ok())?
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if bytes[13] != b':'
        || bytes[16] != b':'
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut rest = &text[19..];
    let mut nanos = 0u32;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        for (index, byte) in fraction.bytes().take(digits.min(9)).enumerate() {
            nanos += u32::from(byte - b'0') * 10u32.pow(8 - index as u32);
        }
        rest = &fraction[digits..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ if rest.len() == 6 && matches!(rest.as_bytes()[0], b'+' | b'-') && rest.as_bytes()[3] == b':' => {
            let hours: i64 = rest[1..3].parse().ok()?;
            let minutes: i64 = rest[4..6].parse().ok()?;
            let sign = if rest.starts_with('-') { -1 } else { 1 };
            sign * (hours * 3600 + minutes * 60)
        }
        _ => return None,
    };
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset;
    Some((seconds, nanos))
}

/// The `since` value one nanosecond after `timestamp`.
pub fn since_after(timestamp: &str) -> Option<String> {
    let (mut seconds, mut nanos) = parse_rfc3339(timestamp)?;
    nanos += 1;
    if nanos == 1_000_000_000 {
        seconds += 1;
        nanos = 0;
    }
    Some(format!("{seconds}.{nanos:09}"))
}

/// The `since` value one nanosecond after an event's `timeNano`.
pub fn since_after_nanos(time_nano: i64) -> String {
    let next = time_nano + 1;
    format!(
        "{}.{:09}",
        next.div_euclid(1_000_000_000),
        next.rem_euclid(1_000_000_000)
    )
}

/// Days from 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_docker_timestamps() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some((0, 0)));
        assert_eq!(
            parse_rfc3339("2024-05-01T12:34:56.123456789Z"),
            Some((1_714_566_896, 123_456_789))
        );
        assert_eq!(
            parse_rfc3339("2024-05-01T14:34:56.5+02:00"),
            Some((1_714_566_896, 500_000_000))
        );
        assert_eq!(parse_rfc3339("2000-02-29T23:59:59Z"), Some((951_868_799, 0)));
        assert_eq!(parse_rfc3339("not a time at all"), None);
        assert_eq!(parse_rfc3339("2024-05-01T12:34:56."), None);
    }

    #[test]
    fn resumes_one_nanosecond_later() {
        assert_eq!(
            since_after("2024-05-01T12:34:56.123456789Z").as_deref(),
            Some("1714566896.123456790")
        );
        assert_eq!(
            since_after("2024-05-01T12:34:56.999999999Z").as_deref(),
            Some("1714566897.000000000")
        );
        assert_eq!(since_after_nanos(1_714_566_896_999_999_999), "1714566897.000000000");
    }
}
