//! Time tags of sequence commands, read as `datetime.strptime` reads them in the Python tool.

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

/// `text` is `HH:MM:SS[.ffffff]`, or `YYYY-DDDT` before it when `absolute`; the lexer
/// guarantees the shape. The error is the message of the Python exception.
pub fn parse(text: &str, absolute: bool) -> Result<Time, String> {
    let fraction = text.contains('.');
    let format = match (absolute, fraction) {
        (false, false) => "%H:%M:%S",
        (false, true) => "%H:%M:%S.%f",
        (true, false) => "%Y-%jT%H:%M:%S",
        (true, true) => "%Y-%jT%H:%M:%S.%f",
    };
    let no_match = || format!("time data {} does not match format '{format}'", crate::value::str_repr(text));
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
    if hour > 23 || minute > 59 {
        return Err(no_match());
    }
    // `%S` takes 00 to 61; of a larger number it takes the first digit only.
    if second > 61 {
        return Err(match fraction {
            true => no_match(),
            false => format!("unconverted data remains: {}", second % 10),
        });
    }
    let mut days = 0;
    if let Some(date) = date {
        let (year, day) = date.split_once('-').ok_or_else(no_match)?;
        let (year, day) = (number(year).ok_or_else(no_match)?, number(day).ok_or_else(no_match)?);
        if !(1..=366).contains(&day) {
            return Err(no_match());
        }
        if year == 0 {
            return Err("year 0 is out of range".into());
        }
        if year == 9999 && day > 365 {
            return Err("date value out of range".into());
        }
        // Day 366 of a year of 365 days is the first day of the next year.
        days = days_before_year(year) + day - 1;
    }
    if second > 59 {
        return Err("second must be in 0..59".into());
    }
    let total = ((days * 24 + hour) * 60 + minute) * 60 + second;
    // The Python tool goes through a float of seconds, which loses some microseconds.
    let delta = (total * 1_000_000 + micros) as f64 / 1e6;
    let seconds = delta.trunc();
    let useconds = ((delta - seconds) * 1e6).trunc();
    if !(0.0..=f64::from(u32::MAX)).contains(&seconds) || useconds < 0.0 {
        return Err(format!("time {text} is outside the range of a sequence time tag"));
    }
    Ok(Time { seconds: seconds as u32, useconds: useconds as u32 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str, absolute: bool) -> (u32, u32) {
        let t = parse(text, absolute).unwrap();
        (t.seconds, t.useconds)
    }

    // Values printed by Python 3.12 for the same texts.
    #[test]
    fn as_python() {
        assert_eq!(ok("23:59:59.999999", false), (86399, 999999));
        assert_eq!(ok("00:00:01.1", false), (1, 100000));
        assert_eq!(ok("00:00:01.000001", false), (1, 0));
        assert_eq!(ok("10:00:00.29", false), (36000, 290000));
        assert_eq!(ok("2023-366T00:00:00", true), (1704067200, 0));
        assert_eq!(ok("2024-366T00:00:00", true), (1735603200, 0));
        assert_eq!(ok("2015-075T22:32:40.123", true), (1426545160, 122999));
        assert_eq!(ok("1970-001T00:00:00.000001", true), (0, 1));
        let err = |text, absolute| parse(text, absolute).unwrap_err();
        assert_eq!(err("00:00:60", false), "second must be in 0..59");
        assert_eq!(err("00:00:62", false), "unconverted data remains: 2");
        assert_eq!(err("24:00:00", false), "time data '24:00:00' does not match format '%H:%M:%S'");
        assert_eq!(err("0000-001T00:00:00", true), "year 0 is out of range");
        assert_eq!(
            err("2023-367T00:00:00", true),
            "time data '2023-367T00:00:00' does not match format '%Y-%jT%H:%M:%S'"
        );
        assert!(parse("1969-365T23:59:59.5", true).is_err());
        assert!(parse("9999-365T23:59:59", true).is_err());
    }
}
