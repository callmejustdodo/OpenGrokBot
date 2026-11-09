//! Invitation codes such as `7F3K-Q9PX`: eight symbols of Crockford's base32 (no
//! `I`, `L`, `O`, or `U`), so a code survives being read aloud or retyped. That is
//! 40 bits from the system's random source; with single use, a 24-hour life, and
//! rate-limited bad attempts, guessing one is out of reach.

use formation_proto::common::{ApiError, ErrorCode};

use crate::ports::Result;

/// Crockford's base32 alphabet. 32 symbols, so one random byte masked to 5 bits
/// picks each symbol with equal chance.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const SYMBOLS: usize = 8;

/// A new code in its canonical form, `XXXX-XXXX`.
pub fn generate() -> Result<String> {
    let mut bytes = [0u8; SYMBOLS];
    getrandom::fill(&mut bytes).map_err(|error| {
        ApiError::new(
            ErrorCode::Internal,
            format!("no system randomness for an invitation code: {error}"),
        )
    })?;
    let symbols: Vec<u8> = bytes
        .iter()
        .map(|byte| ALPHABET[usize::from(byte & 0x1f)])
        .collect();
    Ok(canonical(&symbols))
}

/// The canonical form of what someone typed: case, spaces, and hyphens do not
/// matter, and `O`, `I`, and `L` read as `0`, `1`, and `1`. `None` when it is not
/// eight symbols of the alphabet.
pub fn normalize(input: &str) -> Option<String> {
    let mut symbols = Vec::with_capacity(SYMBOLS);
    for c in input.chars() {
        let symbol = match c.to_ascii_uppercase() {
            ' ' | '-' => continue,
            'O' => b'0',
            'I' | 'L' => b'1',
            c if c.is_ascii() && ALPHABET.contains(&(c as u8)) => c as u8,
            _ => return None,
        };
        symbols.push(symbol);
        if symbols.len() > SYMBOLS {
            return None;
        }
    }
    (symbols.len() == SYMBOLS).then(|| canonical(&symbols))
}

fn canonical(symbols: &[u8]) -> String {
    let (left, right) = symbols.split_at(SYMBOLS / 2);
    format!(
        "{}-{}",
        String::from_utf8_lossy(left),
        String::from_utf8_lossy(right)
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn generated_codes_look_like_the_mockup() {
        let code = generate().unwrap();
        assert_eq!(code.len(), 9);
        assert_eq!(code.as_bytes()[4], b'-');
        assert!(
            code.bytes()
                .filter(|&b| b != b'-')
                .all(|b| ALPHABET.contains(&b))
        );
        assert_eq!(normalize(&code).as_deref(), Some(code.as_str()));
    }

    #[test]
    fn generated_codes_differ() {
        let codes: HashSet<String> = (0..200).map(|_| generate().unwrap()).collect();
        assert_eq!(codes.len(), 200);
    }

    #[test]
    fn typing_mistakes_are_forgiven() {
        for typed in [
            "7F3K-Q9PX",
            "7f3k-q9px",
            " 7F3K Q9PX ",
            "7F3KQ9PX",
            "7f3k - q9px",
        ] {
            assert_eq!(normalize(typed).as_deref(), Some("7F3K-Q9PX"), "{typed:?}");
        }
        assert_eq!(normalize("OIL0-0000").as_deref(), Some("0110-0000"));
    }

    #[test]
    fn other_input_is_not_a_code() {
        for typed in [
            "",
            "7F3K-Q9P",
            "7F3K-Q9PXA",
            "7F3K-Q9PU",
            "7F3K_Q9PX",
            "7F3K-Q9P\u{0130}",
            "formation://join/7F3K-Q9PX",
        ] {
            assert_eq!(normalize(typed), None, "{typed:?}");
        }
    }
}
