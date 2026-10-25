//! Site paths as keys under a host's root.

use crate::error::{Result, SiteError};

/// The key under a host's root that serves `path`: no leading `/`, and `index.html` for a
/// directory path. Paths that would leave the root are refused.
pub(crate) fn site_key(host: &str, path: &str) -> Result<String> {
    let trimmed = path.trim_start_matches('/');
    let mut key = trimmed.to_string();
    if trimmed.is_empty() || trimmed.ends_with('/') {
        key.push_str("index.html");
    }
    if key.contains('\\')
        || key.contains('\0')
        || key.split('/').any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(SiteError::host(host, format!("path '{path}' leaves the site root")));
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::site_key;

    proptest! {
        #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

        /// No path maps to a key outside the root.
        #[test]
        fn no_key_leaves_the_root(path in "[a-z./\\\\]{0,24}") {
            if let Ok(key) = site_key("h", &path) {
                prop_assert!(!key.starts_with('/'));
                prop_assert!(key.split('/').all(|s| !s.is_empty() && s != "." && s != ".."));
                prop_assert!(!key.contains('\\'));
            }
        }
    }

    #[test]
    fn keys_stay_under_the_root() {
        assert_eq!(site_key("h", "/").unwrap(), "index.html");
        assert_eq!(site_key("h", "/a/").unwrap(), "a/index.html");
        assert_eq!(site_key("h", "/a/b.html").unwrap(), "a/b.html");
        assert_eq!(
            site_key("h", ".well-known/awp/log/checkpoint").unwrap(),
            ".well-known/awp/log/checkpoint"
        );
        for bad in ["/../etc/passwd", "/a/../../b", "/a//b", "/./a", "/a\\b"] {
            assert!(site_key("h", bad).is_err(), "{bad}");
        }
    }
}
