//! Forward-compatible deserialization for the API's internally tagged enums.

/// Implements `Serialize` and `Deserialize` for an internally tagged enum whose
/// `type` values the API extends over time.
///
/// The enum derives both traits with `#[serde(remote = "Self", tag = "type")]`,
/// which turns the derived code into inherent functions, and carries an
/// `Unknown(serde_json::Value)` variant marked `#[serde(skip)]`. A `type` listed
/// here goes through the derived code, so a malformed known value still fails
/// with its specific error; any other `type` becomes `Unknown` with the raw JSON,
/// and `Unknown` serializes back to exactly that JSON.
macro_rules! tagged_with_unknown {
    ($ty:ident, $what:literal, [$($tag:literal),+ $(,)?]) => {
        impl $ty {
            /// The `type` values this client deserializes into a typed variant;
            /// any other value becomes the `Unknown` variant.
            pub const KNOWN_TYPES: &'static [&'static str] = &[$($tag),+];
        }

        impl serde::Serialize for $ty {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                match self {
                    Self::Unknown(value) => serde::Serialize::serialize(value, serializer),
                    known => Self::serialize(known, serializer),
                }
            }
        }

        impl<'de> serde::Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                use serde::de::Error as _;
                let value: serde_json::Value = serde::Deserialize::deserialize(deserializer)?;
                match value.get("type").and_then(serde_json::Value::as_str) {
                    Some(tag) if Self::KNOWN_TYPES.contains(&tag) => {
                        Self::deserialize(value).map_err(D::Error::custom)
                    }
                    Some(_) => Ok(Self::Unknown(value)),
                    None => Err(D::Error::custom(concat!($what, " has no string `type` field"))),
                }
            }
        }
    };
}

pub(crate) use tagged_with_unknown;

/// Returns the variant names serde lists in an `unknown variant` error, so a
/// test can check `KNOWN_TYPES` against the enum's derived variants.
#[cfg(test)]
pub(crate) fn expected_variants(error: &str) -> Vec<String> {
    let expected = error.split_once("expected").map_or("", |(_, rest)| rest);
    let mut variants: Vec<String> =
        expected.split('`').skip(1).step_by(2).map(String::from).collect();
    variants.sort();
    variants
}
