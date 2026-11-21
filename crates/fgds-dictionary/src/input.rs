//! Command arguments given as text, read as the Python GDS reads them: `int(text, 0)`, `float(text)`
//! and its boolean words (`CmdData.convert_arg_value`). `fixtures/command-args/table.json` holds what the
//! Python GDS does with each text; the tests of this crate go through it.

/// The first code point of each run of ten decimal digits beyond ASCII (Unicode 15.0.0, category Nd).
const DIGIT_ZEROS: &[u32] = &[
    0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66, 0xDE6, 0xE50, 0xED0, 0xF20,
    0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90, 0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0,
    0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0, 0xFF10, 0x104A0, 0x10D30, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0,
    0x11450, 0x114D0, 0x11650, 0x116C0, 0x11730, 0x118E0, 0x11950, 0x11C50, 0x11D50, 0x11DA0, 0x11F50, 0x16A60,
    0x16AC0, 0x16B50, 0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E4F0, 0x1E950, 0x1FBF0,
];

/// What Python `str.strip()` removes.
fn is_space(c: char) -> bool {
    matches!(c, '\u{9}'..='\u{d}' | '\u{1c}'..='\u{20}' | '\u{85}') || (c.is_whitespace() && !c.is_ascii())
}

/// The text with decimal digits of any script as ASCII digits, and without the white space around it.
fn ascii(text: &str) -> String {
    let digit = |c: char| {
        let code = c as u32;
        match DIGIT_ZEROS.binary_search(&code) {
            Ok(_) => '0',
            Err(0) => c,
            Err(at) if code - DIGIT_ZEROS[at - 1] < 10 => char::from(b'0' + (code - DIGIT_ZEROS[at - 1]) as u8),
            Err(_) => c,
        }
    };
    text.trim_matches(is_space).chars().map(digit).collect()
}

/// Digits of `radix` with single underscores between them (and one after a prefix when `lead`).
fn digits(text: &str, radix: u32, lead: bool) -> Option<String> {
    let text = if lead { text.strip_prefix('_').unwrap_or(text) } else { text };
    let groups: Vec<&str> = text.split('_').collect();
    let valid = groups.iter().all(|g| !g.is_empty() && g.chars().all(|c| c.is_digit(radix)));
    valid.then(|| groups.concat())
}

/// Python `int(text, 0)`: sign, then `0x`, `0o`, `0b` digits or a decimal number without leading zeros.
pub fn python_int(text: &str) -> Option<i128> {
    let text = ascii(text);
    let (negative, rest) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(&text)),
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

/// Python `float(text)`: a decimal number, or `inf`, `infinity`, `nan` in any case, with a sign.
pub fn python_float(text: &str) -> Option<f64> {
    let text = ascii(text);
    let (sign, rest) = match text.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, text.strip_prefix('+').unwrap_or(&text)),
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
    format!("{whole}.{fraction}{exponent}").replace(".e", ".0e").parse::<f64>().ok().map(|v| sign * v)
}

/// The boolean words of the Python GDS, in any case and with white space around.
pub fn python_bool(text: &str) -> Option<bool> {
    match text.to_lowercase().trim_matches(is_space) {
        "true" | "yes" => Some(true),
        "false" | "no" => Some(false),
        _ => None,
    }
}
