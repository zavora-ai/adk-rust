//! Properties of the web grammar and screening (spec 3.1 §11 "External content").

#![cfg(feature = "web")]

use adk_harness::web::{
    FetchRefusal, Fetcher, INJECTION_LABEL, INJECTION_PHRASES, NON_PATH_BUDGET, WebConfig,
    domain_matches, has_secret, redact_secrets, screen, url_allowlisted,
};
use proptest::prelude::*;

fn label() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9-]{0,8}[a-z0-9]"
        .prop_filter("no leading or trailing dash", |l| !l.starts_with('-') && !l.ends_with('-'))
}

fn host() -> impl Strategy<Value = String> {
    prop::collection::vec(label(), 1..4).prop_map(|labels| labels.join("."))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// An exact allowlist entry matches its host and no other registrable domain: not a
    /// look-alike with a prefix or suffix, not a subdomain, not a parent.
    #[test]
    fn an_exact_entry_matches_only_its_host(allowed in host(), other in host(), prefix in label()) {
        prop_assert!(domain_matches(&allowed, &allowed));
        let look_alikes = [
            format!("{prefix}{allowed}"),
            format!("{allowed}{prefix}"),
            format!("{prefix}.{allowed}"),
            format!("{allowed}.{prefix}"),
        ];
        for look_alike in &look_alikes {
            prop_assert!(!domain_matches(&allowed, look_alike), "{} matched {}", allowed, look_alike);
        }
        prop_assert_eq!(domain_matches(&allowed, &other), allowed == other);
        let allow = vec![allowed.clone()];
        let own = format!("https://{allowed}/x?y=1");
        let others = format!("https://{other}/");
        let nested = format!("https://{allowed}.{other}/");
        let userinfo = format!("https://{other}/");
        let backslash = format!("https://{allowed}\\{other}/");
        prop_assert!(url_allowlisted(&own, &allow));
        prop_assert_eq!(url_allowlisted(&others, &allow), allowed == other);
        prop_assert!(!url_allowlisted(&nested, &allow));
        prop_assert!(!url_allowlisted(&userinfo, &allow), "user info never matches");
        prop_assert!(!url_allowlisted(&backslash, &allow), "a backslash never matches");
    }

    /// A wildcard entry matches subdomains of its parent and nothing else: never the
    /// parent itself, never a host whose name merely ends with the parent's letters.
    #[test]
    fn a_wildcard_entry_matches_only_true_subdomains(parent in host(), sub in label(), other in host()) {
        let pattern = format!("*.{parent}");
        let child = format!("{sub}.{parent}");
        let grandchild = format!("{sub}.{sub}.{parent}");
        let glued = format!("{sub}{parent}");
        let dotted_parent = format!(".{parent}");
        prop_assert!(domain_matches(&pattern, &child));
        prop_assert!(domain_matches(&pattern, &grandchild));
        prop_assert!(!domain_matches(&pattern, &parent));
        prop_assert!(!domain_matches(&pattern, &glued));
        prop_assert_eq!(
            domain_matches(&pattern, &other),
            other.len() > parent.len() && other.ends_with(&dotted_parent)
        );
    }

    /// A composed URL whose query or fragment carries a secret pattern, or more than the
    /// non-path budget, is refused before any request (I48).
    #[test]
    fn urls_carrying_secrets_or_oversized_queries_are_refused(
        host in host(),
        key in "AKIA[0-9A-Z]{16}",
        token in "sk-[A-Za-z0-9]{24,40}",
        filler in "[a-z0-9]{257,400}",
        clean in "[a-z0-9=&]{0,40}",
    ) {
        let fetcher = Fetcher::new(WebConfig::default());
        for url in [
            format!("https://{host}/path?token={key}"),
            format!("https://{host}/path#{token}"),
            format!("https://{host}/?a=1&api_key={token}"),
        ] {
            prop_assert_eq!(fetcher.check_url(&url).err(), Some(FetchRefusal::SecretInQuery), "{}", url);
        }
        let oversized = format!("https://{host}/?q={filler}");
        prop_assert!(
            matches!(fetcher.check_url(&oversized), Err(FetchRefusal::NonPathBudget { bytes, max }) if bytes > NON_PATH_BUDGET && max == NON_PATH_BUDGET),
            "{}", oversized
        );
        let plain = format!("https://{host}/path?{clean}");
        prop_assert!(fetcher.check_url(&plain).is_ok() || has_secret(&clean), "{}", plain);
    }

    /// No secret pattern survives redaction, and redaction never drops a line.
    #[test]
    fn redaction_removes_every_secret_and_keeps_every_line(
        lines in prop::collection::vec("[a-zA-Z0-9 ,.]{0,40}", 1..8),
        key in "AKIA[0-9A-Z]{16}",
        token in "ghp_[A-Za-z0-9]{36}",
        at in 0usize..8,
    ) {
        let mut text_lines = lines.clone();
        let at = at.min(text_lines.len() - 1);
        text_lines[at] = format!("{} {key} and {token}", text_lines[at]);
        let text = text_lines.join("\n");
        let (redacted, count) = redact_secrets(&text);
        prop_assert!(count >= 2);
        prop_assert!(!has_secret(&redacted));
        prop_assert!(!redacted.contains(&key) && !redacted.contains(&token));
        prop_assert_eq!(redacted.lines().count(), text.lines().count());
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(120))]

    /// A delivered page never carries an unlabelled injection match or an unredacted
    /// secret, and every line of the page is delivered (I47).
    #[test]
    fn delivered_text_labels_every_injection_and_redacts_every_secret(
        lines in prop::collection::vec("[a-zA-Z0-9 ,.]{0,40}", 1..10),
        phrase in prop::sample::select(INJECTION_PHRASES.to_vec()),
        key in "AKIA[0-9A-Z]{16}",
        inject_at in 0usize..10,
        secret_at in 0usize..10,
    ) {
        let mut page = lines.clone();
        let inject_at = inject_at.min(page.len() - 1);
        let secret_at = secret_at.min(page.len() - 1);
        page[inject_at] = format!("{} {phrase} {}", page[inject_at], "please");
        page[secret_at] = format!("{} key={key}", page[secret_at]);
        let text = page.join("\n");
        let screened = tokio::runtime::Runtime::new().unwrap().block_on(screen(&text));
        prop_assert_eq!(screened.text.lines().count(), text.lines().count());
        for (index, line) in screened.text.lines().enumerate() {
            let lower = line.to_ascii_lowercase();
            if INJECTION_PHRASES.iter().any(|p| lower.contains(p)) {
                prop_assert!(line.starts_with(INJECTION_LABEL), "line {index} is unlabelled: {line}");
                prop_assert!(screened.injection_lines.contains(&(index + 1)));
            }
        }
        prop_assert!(!screened.text.contains(&key));
        prop_assert!(!has_secret(&screened.text));
        prop_assert!(screened.redactions >= 1);
        prop_assert!(screened.flagged());
    }
}
