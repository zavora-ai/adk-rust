//! `AWP-Version` negotiation: the highest common major version (spec 3.5 §7.2; AWP 2.0 §18.2,
//! C24).
//!
//! A requester sends the versions it supports, highest first (`AWP-Version: 2.0, 1.0`); the
//! server answers in the highest major both support and names it in `AWP-Version`. Without
//! the field, a route group answers in its own version. When no major is common the answer is
//! `406` with the supported versions.

use awp_types::{AwpVersion, ErrorCode, VERSION_2_0};
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

use super::response::error_response;

/// The `AWP-Version` request and response field.
pub const AWP_VERSION_HEADER: &str = "AWP-Version";

/// Why a request's `AWP-Version` cannot be served (spec 3.5 §7.2).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NegotiationError {
    /// The field is not a comma-separated list of `major.minor` versions.
    #[error("AWP-Version must list versions as `major.minor`, highest first, such as `2.0, 1.0`")]
    Malformed,
    /// No offered major version is supported.
    #[error("no common AWP major version: offered {offered}, supported {supported}")]
    NoCommonMajor {
        /// The offered versions, as sent.
        offered: String,
        /// The supported versions.
        supported: String,
    },
}

/// Parses an `AWP-Version` field: one or more `major.minor` versions separated by commas.
///
/// # Errors
///
/// Returns [`NegotiationError::Malformed`] when an element is not `major.minor` or the list is
/// empty.
///
/// # Example
///
/// ```
/// use adk_awp::v2::negotiation::parse_offered;
/// use awp_types::AwpVersion;
///
/// let offered = parse_offered("2.0, 1.0").unwrap();
/// assert_eq!(offered, vec![AwpVersion { major: 2, minor: 0 }, AwpVersion { major: 1, minor: 0 }]);
/// assert!(parse_offered("two").is_err());
/// ```
pub fn parse_offered(value: &str) -> Result<Vec<AwpVersion>, NegotiationError> {
    let offered = value
        .split(',')
        .map(|element| {
            element.trim().parse::<AwpVersion>().map_err(|_| NegotiationError::Malformed)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if offered.is_empty() { Err(NegotiationError::Malformed) } else { Ok(offered) }
}

/// Selects the version to answer in: the supported version of the highest major any offered
/// version shares, or the highest supported version when nothing was offered.
///
/// # Errors
///
/// Returns [`NegotiationError::NoCommonMajor`] when no offered major is supported.
///
/// # Example
///
/// ```
/// use adk_awp::v2::negotiation::{parse_offered, select};
/// use awp_types::AwpVersion;
///
/// let supported = [AwpVersion { major: 2, minor: 0 }, AwpVersion { major: 1, minor: 0 }];
/// let offered = parse_offered("1.3").unwrap();
/// assert_eq!(select(Some(&offered), &supported).unwrap(), AwpVersion { major: 1, minor: 0 });
/// assert_eq!(select(None, &supported).unwrap(), AwpVersion { major: 2, minor: 0 });
/// assert!(select(Some(&parse_offered("3.0").unwrap()), &supported).is_err());
/// ```
pub fn select(
    offered: Option<&[AwpVersion]>,
    supported: &[AwpVersion],
) -> Result<AwpVersion, NegotiationError> {
    let mut by_major: Vec<AwpVersion> = supported.to_vec();
    by_major.sort_by_key(|version| std::cmp::Reverse(version.major));
    let Some(offered) = offered else {
        return by_major.first().copied().ok_or(NegotiationError::NoCommonMajor {
            offered: String::new(),
            supported: String::new(),
        });
    };
    by_major
        .iter()
        .find(|version| offered.iter().any(|candidate| candidate.major == version.major))
        .copied()
        .ok_or_else(|| NegotiationError::NoCommonMajor {
            offered: offered.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
            supported: header_value(supported),
        })
}

/// Formats versions as an `AWP-Version` value, highest first: `2.0, 1.0`.
pub fn header_value(versions: &[AwpVersion]) -> String {
    let mut sorted = versions.to_vec();
    sorted.sort_by_key(|version| std::cmp::Reverse((version.major, version.minor)));
    sorted.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
}

/// Negotiates against the request's `AWP-Version` field.
///
/// # Errors
///
/// As [`parse_offered`] and [`select`].
pub fn negotiate(
    headers: &HeaderMap,
    supported: &[AwpVersion],
) -> Result<AwpVersion, NegotiationError> {
    match headers.get(AWP_VERSION_HEADER) {
        None => select(None, supported),
        Some(value) => {
            let value = value.to_str().map_err(|_| NegotiationError::Malformed)?;
            let offered = parse_offered(value)?;
            select(Some(&offered), supported)
        }
    }
}

/// Axum middleware for the `/awp/v2/` routes: answers in 2.0, refuses a request whose
/// `AWP-Version` names no 2.x version with `406 version_not_supported`, and marks every
/// response `Vary: AWP-Version` (spec 3.5 §7.2).
pub async fn v2_version_negotiation(request: Request, next: Next) -> Response {
    let supported = [VERSION_2_0];
    let mut response = match negotiate(request.headers(), &supported) {
        Ok(_) => next.run(request).await,
        Err(NegotiationError::Malformed) => {
            error_response(ErrorCode::InvalidRequest, NegotiationError::Malformed.to_string(), None)
        }
        Err(error @ NegotiationError::NoCommonMajor { .. }) => {
            let mut response =
                error_response(ErrorCode::VersionNotSupported, error.to_string(), None);
            *response.status_mut() = StatusCode::NOT_ACCEPTABLE;
            response
        }
    };
    if let Ok(value) = HeaderValue::from_str(&VERSION_2_0.to_string()) {
        response.headers_mut().insert(AWP_VERSION_HEADER, value);
    }
    response.headers_mut().append(header::VARY, HeaderValue::from_static(AWP_VERSION_HEADER));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    const V1: AwpVersion = AwpVersion { major: 1, minor: 0 };
    const V2: AwpVersion = AwpVersion { major: 2, minor: 0 };

    #[test]
    fn the_highest_common_major_wins() {
        let both = [V1, V2];
        let cases = [
            ("2.0, 1.0", Ok(V2)),
            ("1.0, 2.0", Ok(V2)),
            ("2.1", Ok(V2)),
            ("1.4", Ok(V1)),
            (" 1.0 ,  2.0 ", Ok(V2)),
            ("3.0", Err(())),
        ];
        for (offered, expected) in cases {
            let offered = parse_offered(offered).unwrap();
            assert_eq!(select(Some(&offered), &both).map_err(|_| ()), expected);
        }
        assert_eq!(select(Some(&[V2]), &[V1]).map_err(|_| ()), Err(()));
        assert_eq!(select(None, &[V1]), Ok(V1));
    }

    #[test]
    fn malformed_lists_are_refused() {
        for bad in ["", "two", "2", "2.0,", "2.0;q=1", "2.0.1"] {
            assert_eq!(parse_offered(bad), Err(NegotiationError::Malformed), "{bad:?}");
        }
    }

    #[test]
    fn header_values_list_highest_first() {
        assert_eq!(header_value(&[V1, V2]), "2.0, 1.0");
        assert_eq!(header_value(&[V2]), "2.0");
    }
}
