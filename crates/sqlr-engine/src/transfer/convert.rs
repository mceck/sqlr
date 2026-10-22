//! Value transforms of a transfer's column mapping.

use sqlr_core::{Transform, Value};

/// `value` after `transform`. Text is trimmed or recased; anything else passes through
/// (a constant replaces any value).
pub(super) fn apply_transform(value: Value, transform: &Transform) -> Value {
    match (transform, value) {
        (Transform::Constant(text), _) => Value::Text(text.clone()),
        (_, Value::Text(text)) => match transform {
            Transform::Trim => Value::Text(text.trim().to_string()),
            Transform::Upper => Value::Text(text.to_uppercase()),
            Transform::Lower => Value::Text(text.to_lowercase()),
            Transform::NullIfEmpty if text.trim().is_empty() => Value::Null,
            _ => Value::Text(text),
        },
        (_, other) => other,
    }
}

/// `text` (a timestamp with a UTC offset: `2024-03-09 12:34:56.5+02:00`, `…Z`) as the same
/// instant in UTC without an offset, which is what a `datetime` column takes. Text without
/// an offset, or that is not a timestamp, comes back as it is.
pub(super) fn utc_naive(text: &str) -> String {
    fn num(s: &str) -> Option<i64> {
        s.parse().ok()
    }
    let t = text.trim();
    let Some(sep) = t.find(['T', ' ']) else {
        return text.to_string();
    };
    let (date, rest) = (&t[..sep], &t[sep + 1..]);
    // The offset starts at the last `+`, `-` or `Z` of the time part.
    let Some(at) = rest.rfind(['+', '-', 'Z']) else {
        return text.to_string();
    };
    let (clock, offset) = rest.split_at(at);
    let sign = match offset.as_bytes()[0] {
        b'-' => -1,
        _ => 1,
    };
    let digits = offset[1..].replace(':', "");
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return text.to_string();
    }
    let minutes = match digits.len() {
        0 => Some(0),
        2 => num(&digits).map(|h| h * 60),
        4 => num(&digits[..2])
            .zip(num(&digits[2..]))
            .map(|(h, m)| h * 60 + m),
        _ => None,
    };
    let parts: Vec<&str> = date.split('-').collect();
    let (Some(minutes), [y, m, d]) = (minutes, parts.as_slice()) else {
        return text.to_string();
    };
    let (clock_main, frac) = match clock.split_once('.') {
        Some((main, frac)) => (main, Some(frac)),
        None => (clock, None),
    };
    let hms: Vec<&str> = clock_main.split(':').collect();
    let (Some(y), Some(m), Some(d), [h, mi, s]) = (num(y), num(m), num(d), hms.as_slice()) else {
        return text.to_string();
    };
    let (Some(h), Some(mi), Some(s)) = (num(h), num(mi), num(s)) else {
        return text.to_string();
    };
    let seconds = days_from_civil(y, m, d) * 86_400 + h * 3600 + mi * 60 + s - sign * minutes * 60;
    let days = seconds.div_euclid(86_400);
    let of_day = seconds.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let mut out = format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        of_day / 3600,
        of_day / 60 % 60,
        of_day % 60
    );
    if let Some(frac) = frac {
        out.push('.');
        out.push_str(frac);
    }
    out
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The proleptic Gregorian date of a day count since 1970-01-01.
pub(super) fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use sqlr_core::{Transform, Value};

    use super::*;

    #[test]
    fn offsets_are_folded_into_utc_for_naive_datetimes() {
        let utc = |s: &str| utc_naive(s);
        assert_eq!(utc("2024-03-09 12:34:56+00"), "2024-03-09 12:34:56");
        assert_eq!(utc("2024-03-09 12:34:56.5+02:00"), "2024-03-09 10:34:56.5");
        assert_eq!(utc("2024-03-09 00:30:00-01:30"), "2024-03-09 02:00:00");
        assert_eq!(utc("2024-03-09T23:00:00Z"), "2024-03-09 23:00:00");
        // Across midnight, month and year.
        assert_eq!(utc("2024-03-01 01:00:00+02:00"), "2024-02-29 23:00:00");
        assert_eq!(utc("2023-12-31 23:00:00-02:00"), "2024-01-01 01:00:00");
        // An offset that is not digits cannot be folded (and must not be sliced).
        assert_eq!(utc("2024-01-01 10:00:00+1é2"), "2024-01-01 10:00:00+1é2");
        // Nothing to fold, or not a datetime: left to the server.
        assert_eq!(utc("2024-03-09 12:00:00"), "2024-03-09 12:00:00");
        assert_eq!(utc("2024-03-09"), "2024-03-09");
        assert_eq!(utc("soon"), "soon");
    }

    #[test]
    fn transforms_change_values() {
        let t = |s: &str| Value::Text(s.into());
        assert_eq!(apply_transform(t("  a b "), &Transform::Trim), t("a b"));
        assert_eq!(apply_transform(t("aB"), &Transform::Upper), t("AB"));
        assert_eq!(apply_transform(t("aB"), &Transform::Lower), t("ab"));
        assert_eq!(
            apply_transform(t("  "), &Transform::NullIfEmpty),
            Value::Null
        );
        assert_eq!(apply_transform(t(""), &Transform::NullIfEmpty), Value::Null);
        assert_eq!(apply_transform(t(" x"), &Transform::NullIfEmpty), t(" x"));
        // Only text is touched; NULL and numbers pass through.
        assert_eq!(apply_transform(Value::Null, &Transform::Upper), Value::Null);
        assert_eq!(
            apply_transform(Value::Int(7), &Transform::Trim),
            Value::Int(7)
        );
        assert_eq!(
            apply_transform(Value::Int(7), &Transform::Constant("k".into())),
            t("k")
        );
    }
}
