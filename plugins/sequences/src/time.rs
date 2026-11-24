//! Time tags of sequence commands.

/// Seconds and microseconds of a time tag.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Time {
    pub seconds: u32,
    pub useconds: u32,
}

fn number(text: &str) -> Option<i64> {
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())).then(|| text.parse().ok()).flatten()
}

/// Days from 1970-01-01 to January 1st of `year`.
fn days_before_year(year: i64) -> i64 {
    let y = year - 1;
    y * 365 + y / 4 - y / 100 + y / 400 - 719_162
}

fn leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// `text` is `HH:MM:SS[.ffffff]`, or `YYYY-DDDT` before it when `absolute`; the lexer guarantees the shape.
pub fn parse(text: &str, absolute: bool) -> Result<Time, String> {
    let shape = if absolute { "YYYY-DDDTHH:MM:SS[.ffffff]" } else { "HH:MM:SS[.ffffff]" };
    let no_match = || format!("time {text} is not {shape}");
    let (date, clock) = match absolute {
        true => text.split_once('T').map(|(d, c)| (Some(d), c)).ok_or_else(no_match)?,
        false => (None, text),
    };
    let (clock, micros) = match clock.split_once('.') {
        Some((clock, f)) => {
            let digits = number(f).filter(|_| f.len() <= 6).ok_or_else(no_match)?;
            (clock, digits * 10i64.pow(6 - f.len() as u32))
        }
        None => (clock, 0),
    };
    let mut parts = clock.split(':').map(number);
    let (Some(Some(hour)), Some(Some(minute)), Some(Some(second)), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(no_match());
    };
    if hour > 23 || minute > 59 || second > 59 {
        return Err(format!("time {text}: the hour is 0 to 23, the minute and the second 0 to 59"));
    }
    let mut days = 0;
    if let Some(date) = date {
        let (year, day) = date.split_once('-').ok_or_else(no_match)?;
        let (year, day) = (number(year).ok_or_else(no_match)?, number(day).ok_or_else(no_match)?);
        let last = if leap(year) { 366 } else { 365 };
        if !(1..=last).contains(&day) {
            return Err(format!("time {text}: the day of year {year} is 1 to {last}"));
        }
        days = days_before_year(year) + day - 1;
    }
    let seconds = ((days * 24 + hour) * 60 + minute) * 60 + second;
    let seconds =
        u32::try_from(seconds).map_err(|_| format!("time {text} is outside the range of a sequence time tag"))?;
    Ok(Time { seconds, useconds: micros as u32 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str, absolute: bool) -> (u32, u32) {
        let t = parse(text, absolute).unwrap();
        (t.seconds, t.useconds)
    }

    #[test]
    fn time_tags_are_read_exactly() {
        assert_eq!(ok("23:59:59.999999", false), (86399, 999999));
        assert_eq!(ok("00:00:01.1", false), (1, 100000));
        assert_eq!(ok("00:00:01.000001", false), (1, 1));
        assert_eq!(ok("10:00:00.29", false), (36000, 290000));
        assert_eq!(ok("2024-366T00:00:00", true), (1735603200, 0));
        assert_eq!(ok("2015-075T22:32:40.123", true), (1426545160, 123000));
        assert_eq!(ok("1970-001T00:00:00.000001", true), (0, 1));
    }

    #[test]
    fn a_time_that_does_not_exist_is_refused() {
        let err = |text, absolute| parse(text, absolute).unwrap_err();
        assert_eq!(err("2023-366T00:00:00", true), "time 2023-366T00:00:00: the day of year 2023 is 1 to 365");
        assert_eq!(err("2023-000T00:00:00", true), "time 2023-000T00:00:00: the day of year 2023 is 1 to 365");
        assert!(parse("2024-367T00:00:00", true).is_err());
        assert!(parse("00:00:60", false).is_err());
        assert!(parse("24:00:00", false).is_err());
        assert!(parse("00:00:00.1234567", false).is_err());
        assert!(parse("1969-365T23:59:59.5", true).is_err());
        assert!(parse("9999-365T23:59:59", true).is_err());
    }
}
