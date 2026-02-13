/// Generates a hybrid string/numeric serde Deserializer for Gemini API enums.
///
/// Vertex AI returns proto3 numeric codes while Google AI Studio returns
/// SCREAMING_SNAKE_CASE strings. This macro generates both mapping functions
/// and a custom `Deserialize` impl that handles either format gracefully.
///
/// # Usage
/// ```ignore
/// hybrid_enum! {
///     /// Doc comment for the enum
///     pub enum FinishReason {
///         FinishReasonUnspecified => ("FINISH_REASON_UNSPECIFIED", 0),
///         Stop                   => ("STOP", 1),
///         // ...
///     }
///     fallback: Other
/// }
/// ```
macro_rules! hybrid_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $Name:ident {
            $(
                $(#[$vmeta:meta])*
                $Variant:ident => ($wire_str:literal, $wire_num:literal)
            ),+ $(,)?
        }
        fallback: $Fallback:ident
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, ::serde::Serialize, PartialEq)]
        $vis enum $Name {
            $(
                $(#[$vmeta])*
                $Variant,
            )+
        }

        impl $Name {
            fn from_wire_str(value: &str) -> Self {
                match value {
                    $( $wire_str => Self::$Variant, )+
                    _ => Self::$Fallback,
                }
            }

            fn from_wire_number(value: i64) -> Self {
                match value {
                    $( $wire_num => Self::$Variant, )+
                    _ => Self::$Fallback,
                }
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $Name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: ::serde::Deserializer<'de>,
            {
                let value = ::serde_json::Value::deserialize(deserializer)?;
                match value {
                    ::serde_json::Value::String(s) => Ok(Self::from_wire_str(&s)),
                    ::serde_json::Value::Number(n) => {
                        n.as_i64()
                            .map(Self::from_wire_number)
                            .ok_or_else(|| {
                                ::serde::de::Error::custom(
                                    concat!(stringify!($Name), " must be an integer-compatible number")
                                )
                            })
                    }
                    _ => Err(::serde::de::Error::custom(
                        concat!(stringify!($Name), " must be a string or integer")
                    )),
                }
            }
        }
    };
}
