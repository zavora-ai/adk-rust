//! The JWS algorithms a JWKS validator accepts.

use std::fmt;
use std::str::FromStr;

use super::TokenError;

/// A JWS signature algorithm that [`JwtValidator`](super::JwtValidator) can verify against
/// the public keys of a JWKS.
///
/// Only asymmetric RSA and EC algorithms are listed: a symmetric algorithm (`HS256`, ...)
/// would verify with a secret anyone can read from the JWKS, so it cannot be expressed, and
/// `none` is never accepted. The enum is ADK-owned, so a new major release of the JWT
/// library underneath does not change this API.
///
/// # Example
///
/// ```rust
/// use adk_auth::sso::{JwtAlgorithm, JwtValidator};
///
/// let validator = JwtValidator::builder()
///     .issuer("https://issuer.example.com")
///     .audience("https://api.example.com")
///     .jwks_uri("https://issuer.example.com/.well-known/jwks.json")
///     .algorithm(JwtAlgorithm::RS256)
///     .algorithm("ES256".parse::<JwtAlgorithm>()?)
///     .build()?;
///
/// assert!("HS256".parse::<JwtAlgorithm>().is_err());
/// # Ok::<(), adk_auth::sso::TokenError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum JwtAlgorithm {
    /// RSASSA-PKCS1-v1_5 with SHA-256.
    RS256,
    /// RSASSA-PKCS1-v1_5 with SHA-384.
    RS384,
    /// RSASSA-PKCS1-v1_5 with SHA-512.
    RS512,
    /// RSASSA-PSS with SHA-256.
    PS256,
    /// RSASSA-PSS with SHA-384.
    PS384,
    /// RSASSA-PSS with SHA-512.
    PS512,
    /// ECDSA on P-256 with SHA-256.
    ES256,
    /// ECDSA on P-384 with SHA-384.
    ES384,
}

impl JwtAlgorithm {
    /// The algorithm's JWS name, as it appears in a token's `alg` header.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_auth::sso::JwtAlgorithm;
    ///
    /// assert_eq!(JwtAlgorithm::PS256.as_str(), "PS256");
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RS256 => "RS256",
            Self::RS384 => "RS384",
            Self::RS512 => "RS512",
            Self::PS256 => "PS256",
            Self::PS384 => "PS384",
            Self::PS512 => "PS512",
            Self::ES256 => "ES256",
            Self::ES384 => "ES384",
        }
    }

    pub(crate) fn to_jsonwebtoken(self) -> jsonwebtoken::Algorithm {
        match self {
            Self::RS256 => jsonwebtoken::Algorithm::RS256,
            Self::RS384 => jsonwebtoken::Algorithm::RS384,
            Self::RS512 => jsonwebtoken::Algorithm::RS512,
            Self::PS256 => jsonwebtoken::Algorithm::PS256,
            Self::PS384 => jsonwebtoken::Algorithm::PS384,
            Self::PS512 => jsonwebtoken::Algorithm::PS512,
            Self::ES256 => jsonwebtoken::Algorithm::ES256,
            Self::ES384 => jsonwebtoken::Algorithm::ES384,
        }
    }
}

impl fmt::Display for JwtAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for JwtAlgorithm {
    type Err = TokenError;

    /// Parses a JWS algorithm name, matching case exactly as JWS does.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::UnsupportedAlgorithm`] for a symmetric algorithm, `none`, EdDSA,
    /// or any name that is not an RSA or EC algorithm listed above.
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "RS256" => Ok(Self::RS256),
            "RS384" => Ok(Self::RS384),
            "RS512" => Ok(Self::RS512),
            "PS256" => Ok(Self::PS256),
            "PS384" => Ok(Self::PS384),
            "PS512" => Ok(Self::PS512),
            "ES256" => Ok(Self::ES256),
            "ES384" => Ok(Self::ES384),
            other => Err(TokenError::UnsupportedAlgorithm(format!(
                "'{other}' cannot be verified against a JWKS. Use one of RS256, RS384, RS512, PS256, PS384, PS512, ES256 or ES384."
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::JwtAlgorithm;
    use crate::sso::TokenError;

    const ALL: [JwtAlgorithm; 8] = [
        JwtAlgorithm::RS256,
        JwtAlgorithm::RS384,
        JwtAlgorithm::RS512,
        JwtAlgorithm::PS256,
        JwtAlgorithm::PS384,
        JwtAlgorithm::PS512,
        JwtAlgorithm::ES256,
        JwtAlgorithm::ES384,
    ];

    #[test]
    fn every_algorithm_round_trips_through_its_jws_name() {
        for algorithm in ALL {
            assert_eq!(algorithm.as_str().parse::<JwtAlgorithm>().unwrap(), algorithm);
            assert_eq!(algorithm.to_string(), algorithm.as_str());
            assert_eq!(format!("{:?}", algorithm.to_jsonwebtoken()), algorithm.as_str());
        }
    }

    #[test]
    fn symmetric_none_and_unknown_names_are_refused() {
        for name in ["HS256", "HS384", "HS512", "none", "None", "EdDSA", "rs256", ""] {
            let refused = name.parse::<JwtAlgorithm>();
            assert!(matches!(&refused, Err(TokenError::UnsupportedAlgorithm(_))), "{name}");
        }
    }
}
