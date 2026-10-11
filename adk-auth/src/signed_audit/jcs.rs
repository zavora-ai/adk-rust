//! RFC 8785 JSON Canonicalization Scheme (JCS).
//!
//! Object members are sorted by the UTF-16 code units of their names, strings use the
//! ECMAScript `JSON.stringify` escapes, and numbers use the ECMAScript `Number`
//! serialization. Integers outside ±2^53 are refused, as I-JSON requires, because
//! they cannot round-trip through an IEEE 754 double.

use serde_json::Value;

/// Largest integer every IEEE 754 double represents exactly.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// A value that has no RFC 8785 canonical form.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cannot canonicalize JSON: {0}. Keep integers within ±2^53 or store them as strings")]
pub struct CanonicalizeError(String);

/// Returns the RFC 8785 canonical form of `value`.
///
/// # Example
///
/// ```rust
/// use adk_auth::signed_audit::canonicalize;
/// use serde_json::json;
///
/// let text = canonicalize(&json!({ "b": [1.50, true], "a": "€" })).unwrap();
/// assert_eq!(text, r#"{"a":"€","b":[1.5,true]}"#);
/// ```
///
/// # Errors
///
/// Returns [`CanonicalizeError`] for an integer outside ±2^53.
pub fn canonicalize(value: &Value) -> Result<String, CanonicalizeError> {
    let mut out = String::new();
    write_value(value, &mut out)?;
    Ok(out)
}

fn write_value(value: &Value, out: &mut String) -> Result<(), CanonicalizeError> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => {
            let float = if let Some(unsigned) = number.as_u64() {
                if unsigned > MAX_SAFE_INTEGER {
                    return Err(CanonicalizeError(format!("integer {unsigned} exceeds 2^53")));
                }
                unsigned as f64
            } else if let Some(signed) = number.as_i64() {
                if signed.unsigned_abs() > MAX_SAFE_INTEGER {
                    return Err(CanonicalizeError(format!("integer {signed} exceeds 2^53")));
                }
                signed as f64
            } else {
                number
                    .as_f64()
                    .ok_or_else(|| CanonicalizeError(format!("number {number} is not finite")))?
            };
            // ECMAScript prints negative zero as "0".
            let float = if float == 0.0 { 0.0 } else { float };
            out.push_str(ryu_js::Buffer::new().format_finite(float));
        }
        Value::String(text) => write_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(item, out)?;
            }
            out.push(']');
        }
        Value::Object(members) => {
            let mut sorted: Vec<(&String, &Value)> = members.iter().collect();
            sorted.sort_by(|(left, _), (right, _)| left.encode_utf16().cmp(right.encode_utf16()));
            out.push('{');
            for (index, (name, member)) in sorted.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_string(name, out);
                out.push(':');
                write_value(member, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canon(json: &str) -> String {
        canonicalize(&serde_json::from_str(json).unwrap()).unwrap()
    }

    /// RFC 8785 section 3.2.2.
    #[test]
    fn rfc8785_primitive_data_types() {
        let input = r#"{
          "numbers": [333333333.33333329, 1E30, 4.50, 2e-3, 0.000000000000000000000000001],
          "string": "\u20ac$\u000F\u000aA'\u0042\u0022\u005c\\\"\/",
          "literals": [null, true, false]
        }"#;
        assert_eq!(
            canon(input),
            r#"{"literals":[null,true,false],"numbers":[333333333.3333333,1e+30,4.5,0.002,1e-27],"string":"€$\u000f\nA'B\"\\\\\"/"}"#
        );
    }

    /// RFC 8785 section 3.2.3: members sort by UTF-16 code units, so the emoji
    /// (a surrogate pair starting 0xD83D) precedes U+FB33.
    #[test]
    fn rfc8785_sorting() {
        let input = r#"{
          "\u20ac": "Euro Sign",
          "\r": "Carriage Return",
          "\ufb33": "Hebrew Letter Dalet With Dagesh",
          "1": "One",
          "\ud83d\ude00": "Emoji: Grinning Face",
          "\u0080": "Control",
          "\u00f6": "Latin Small Letter O With Diaeresis"
        }"#;
        assert_eq!(
            canon(input),
            "{\"\\r\":\"Carriage Return\",\"1\":\"One\",\"\u{80}\":\"Control\",\
             \"ö\":\"Latin Small Letter O With Diaeresis\",\"€\":\"Euro Sign\",\
             \"😀\":\"Emoji: Grinning Face\",\"\u{fb33}\":\"Hebrew Letter Dalet With Dagesh\"}"
        );
    }

    /// RFC 8785 appendix B number samples, given as IEEE 754 bit patterns.
    #[test]
    fn rfc8785_numbers() {
        let cases: &[(u64, &str)] = &[
            (0x0000_0000_0000_0000, "0"),
            (0x8000_0000_0000_0000, "0"),
            (0x0000_0000_0000_0001, "5e-324"),
            (0x8000_0000_0000_0001, "-5e-324"),
            (0x7fef_ffff_ffff_ffff, "1.7976931348623157e+308"),
            (0xffef_ffff_ffff_ffff, "-1.7976931348623157e+308"),
            (0x4340_0000_0000_0000, "9007199254740992"),
            (0xc340_0000_0000_0000, "-9007199254740992"),
            (0x4430_0000_0000_0000, "295147905179352830000"),
            (0x44b5_2d02_c7e1_4af5, "9.999999999999997e+22"),
            (0x44b5_2d02_c7e1_4af6, "1e+23"),
            (0x44b5_2d02_c7e1_4af7, "1.0000000000000001e+23"),
            (0x444b_1ae4_d6e2_ef4e, "999999999999999700000"),
            (0x444b_1ae4_d6e2_ef4f, "999999999999999900000"),
            (0x444b_1ae4_d6e2_ef50, "1e+21"),
            (0x3eb0_c6f7_a0b5_ed8c, "9.999999999999997e-7"),
            (0x3eb0_c6f7_a0b5_ed8d, "0.000001"),
            (0x41b3_de43_5555_5553, "333333333.3333332"),
            (0x41b3_de43_5555_5554, "333333333.33333325"),
            (0x41b3_de43_5555_5555, "333333333.3333333"),
            (0x41b3_de43_5555_5556, "333333333.3333334"),
            (0x41b3_de43_5555_5557, "333333333.33333343"),
            (0xbecb_f647_612f_3696, "-0.0000033333333333333333"),
            (0x4314_3ff3_c1cb_0959, "1424953923781206.2"),
        ];
        for (bits, expected) in cases {
            let value = serde_json::Number::from_f64(f64::from_bits(*bits)).map(Value::Number);
            assert_eq!(canonicalize(&value.unwrap()).unwrap(), *expected, "bits {bits:#018x}");
        }
    }

    #[test]
    fn integers_beyond_two_to_the_53_are_refused() {
        assert_eq!(canon("9007199254740991"), "9007199254740991");
        assert_eq!(canon("-9007199254740991"), "-9007199254740991");
        assert!(canonicalize(&serde_json::json!(9_007_199_254_740_993_u64)).is_err());
        assert!(canonicalize(&serde_json::json!(-9_007_199_254_740_993_i64)).is_err());
    }

    #[test]
    fn canonical_text_is_a_fixed_point() {
        let text = canon(r#"{"z":{"b":[1,2.0,-0.0],"a":"\u0001"},"a":[]}"#);
        assert_eq!(text, r#"{"a":[],"z":{"a":"\u0001","b":[1,2,0]}}"#);
        assert_eq!(canon(&text), text);
    }
}
