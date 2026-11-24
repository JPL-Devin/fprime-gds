//! Command arguments given as text. `fixtures/command-args/table.json` holds the bytes the Python GDS
//! sends for each text; the tests of this crate go through it.

/// The text without the ASCII white space around it.
fn ascii(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_ascii_whitespace())
}

/// Digits of `radix` with single underscores between them (and one after a prefix when `lead`).
fn digits(text: &str, radix: u32, lead: bool) -> Option<String> {
    let text = if lead { text.strip_prefix('_').unwrap_or(text) } else { text };
    let groups: Vec<&str> = text.split('_').collect();
    let valid = groups.iter().all(|g| !g.is_empty() && g.chars().all(|c| c.is_digit(radix)));
    valid.then(|| groups.concat())
}

/// An integer: sign, then `0x`, `0o`, `0b` digits or a decimal number without leading zeros.
pub fn integer(text: &str) -> Option<i128> {
    let text = ascii(text);
    let (negative, rest) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let prefixed = |radix: u32, lower: &str, upper: &str| {
        let body = rest.strip_prefix(lower).or_else(|| rest.strip_prefix(upper))?;
        Some(digits(body, radix, true).and_then(|d| i128::from_str_radix(&d, radix).ok()))
    };
    let value = prefixed(16, "0x", "0X").or_else(|| prefixed(8, "0o", "0O")).or_else(|| prefixed(2, "0b", "0B"));
    let value = match value {
        Some(value) => value?,
        None => {
            let d = digits(rest, 10, false)?;
            // A leading zero is allowed only when every digit is zero.
            if d.starts_with('0') && d.chars().any(|c| c != '0') {
                return None;
            }
            // Beyond 128 bits no integer type holds it: any such value is out of range.
            d.parse().unwrap_or(i128::MAX)
        }
    };
    Some(if negative { -value } else { value })
}

/// A float: a decimal number, or `inf`, `infinity`, `nan` in any case, with a sign.
pub fn float(text: &str) -> Option<f64> {
    let text = ascii(text);
    let (sign, rest) = match text.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, text.strip_prefix('+').unwrap_or(text)),
    };
    match rest.to_ascii_lowercase().as_str() {
        "inf" | "infinity" => return Some(sign * f64::INFINITY),
        "nan" => return Some(f64::NAN.copysign(sign)),
        _ => {}
    }
    let (mantissa, exponent) = match rest.find(['e', 'E']) {
        Some(at) => (&rest[..at], Some(&rest[at + 1..])),
        None => (rest, None),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let part = |text: &str| if text.is_empty() { Some(String::new()) } else { digits(text, 10, false) };
    let (whole, fraction) = (part(whole)?, part(fraction)?);
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    let exponent = match exponent {
        Some(e) => {
            let (minus, e) = match e.strip_prefix('-') {
                Some(e) => ("-", e),
                None => ("", e.strip_prefix('+').unwrap_or(e)),
            };
            format!("e{minus}{}", digits(e, 10, false)?)
        }
        None => String::new(),
    };
    // A literal beyond a double is refused: only `inf` spelled out is infinity.
    let value = format!("{whole}.{fraction}{exponent}").replace(".e", ".0e").parse::<f64>().ok()?;
    value.is_finite().then_some(sign * value)
}

/// The boolean words, in any case and with white space around.
pub fn boolean(text: &str) -> Option<bool> {
    match text.to_lowercase().trim() {
        "true" | "yes" => Some(true),
        "false" | "no" => Some(false),
        _ => None,
    }
}
