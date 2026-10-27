//! State values as the observer shows them, and what changed between two states
//! (spec 3.3 §12, §11 S-I9).
//!
//! Every value shown passes, in order: secret redaction (`adk_harness::web::has_secret`), the
//! host's hidden keys, then a size cap. Values are never logged.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::view::{Change, ChannelValue, StateChange};
use super::words;

/// The largest value shown in full, in bytes of compact JSON.
pub const VALUE_CAP: usize = 16 * 1024;

/// What became of a value on its way to the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Shown {
    AsIs,
    Redacted,
    Hidden,
    Capped,
}

impl Shown {
    fn note(self) -> Option<String> {
        match self {
            Self::AsIs => None,
            Self::Redacted => Some(words::LOOKS_LIKE_A_SECRET.to_string()),
            Self::Hidden => Some(words::HIDDEN_KEY.to_string()),
            Self::Capped => Some(words::TOO_LARGE.to_string()),
        }
    }
}

/// How values are shown: the keys the host hides, matched at any depth, without case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Display {
    hidden: BTreeSet<String>,
}

impl Display {
    pub(crate) fn new(hidden: impl IntoIterator<Item = String>) -> Self {
        Self { hidden: hidden.into_iter().map(|key| key.to_ascii_lowercase()).collect() }
    }

    fn hides(&self, key: &str) -> bool {
        self.hidden.contains(&key.to_ascii_lowercase())
    }

    /// `value` under `key` as the observer shows it, with why it differs.
    fn show(&self, key: &str, value: &Value) -> (Value, Shown) {
        let mut shown = Shown::AsIs;
        let mut value = redact(key, value, &mut shown);
        if self.hides(key) {
            value = Value::String(words::HIDDEN.to_string());
            shown = shown.max(Shown::Hidden);
        } else {
            value = self.hide_nested(value, &mut shown);
        }
        let text = value.to_string();
        if text.len() > VALUE_CAP {
            let mut cut: String = text.chars().take(VALUE_CAP).collect();
            cut.push('…');
            value = Value::String(cut);
            shown = shown.max(Shown::Capped);
        }
        (value, shown)
    }

    fn hide_nested(&self, value: Value, shown: &mut Shown) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(key, value)| {
                        if self.hides(&key) {
                            *shown = (*shown).max(Shown::Hidden);
                            (key, Value::String(words::HIDDEN.to_string()))
                        } else {
                            let value = self.hide_nested(value, shown);
                            (key, value)
                        }
                    })
                    .collect(),
            ),
            Value::Array(items) => {
                Value::Array(items.into_iter().map(|item| self.hide_nested(item, shown)).collect())
            }
            other => other,
        }
    }

    /// One channel's value at a checkpoint.
    pub(crate) fn channel(&self, key: &str, value: Option<&Value>) -> ChannelValue {
        let (value, shown) = self.show(key, value.unwrap_or(&Value::Null));
        ChannelValue { key: key.to_string(), value, note: shown.note() }
    }

    /// What changed from `before` to `after`, by key, as the observer shows it.
    pub(crate) fn changes(
        &self,
        before: &BTreeMap<String, Value>,
        after: &BTreeMap<String, Value>,
    ) -> Vec<StateChange> {
        diff(before, after)
            .into_iter()
            .map(|raw| {
                let before = raw.before.as_ref().map(|value| self.show(&raw.key, value));
                let after = raw.after.as_ref().map(|value| self.show(&raw.key, value));
                let shown = before
                    .iter()
                    .chain(after.iter())
                    .map(|(_, shown)| *shown)
                    .max()
                    .unwrap_or(Shown::AsIs);
                StateChange {
                    change_text: words::change(raw.change).to_string(),
                    key: raw.key,
                    change: raw.change,
                    before: before.map(|(value, _)| value),
                    after: after.map(|(value, _)| value),
                    note: shown.note(),
                }
            })
            .collect()
    }
}

/// A string with a secret pattern, alone or after its key (`token=…`), reads `[redacted]`.
fn redact(key: &str, value: &Value, shown: &mut Shown) -> Value {
    match value {
        Value::String(text) => {
            let secret = adk_harness::web::has_secret(text)
                || adk_harness::web::has_secret(&format!("{key}={text}"));
            if secret {
                *shown = (*shown).max(Shown::Redacted);
                Value::String(words::REDACTED.to_string())
            } else {
                value.clone()
            }
        }
        Value::Object(map) => Value::Object(
            map.iter().map(|(inner, value)| (inner.clone(), redact(inner, value, shown))).collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| redact(key, item, shown)).collect())
        }
        other => other.clone(),
    }
}

/// One key's change, before any value is redacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawChange {
    pub(crate) key: String,
    pub(crate) change: Change,
    pub(crate) before: Option<Value>,
    pub(crate) after: Option<Value>,
}

/// What changed from `before` to `after`, sorted by key.
pub(crate) fn diff(
    before: &BTreeMap<String, Value>,
    after: &BTreeMap<String, Value>,
) -> Vec<RawChange> {
    let keys: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    keys.into_iter()
        .filter_map(|key| {
            let (old, new) = (before.get(key), after.get(key));
            let change = match (old, new) {
                (None, Some(_)) => Change::Added,
                (Some(_), None) => Change::Removed,
                (Some(old), Some(new)) if old != new => Change::Changed,
                _ => return None,
            };
            Some(RawChange { key: key.clone(), change, before: old.cloned(), after: new.cloned() })
        })
        .collect()
}

/// `before` with `changes` applied: equals the `after` they were computed from.
#[cfg(test)]
pub(crate) fn apply(
    before: &BTreeMap<String, Value>,
    changes: &[RawChange],
) -> BTreeMap<String, Value> {
    let mut state = before.clone();
    for change in changes {
        match &change.after {
            Some(value) => {
                state.insert(change.key.clone(), value.clone());
            }
            None => {
                state.remove(&change.key);
            }
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;

    fn state(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs.iter().map(|(key, value)| ((*key).to_string(), value.clone())).collect()
    }

    #[test]
    fn changes_are_added_changed_and_removed() {
        let before =
            state(&[("invoice", json!({ "status": "ready" })), ("note", json!("Bill Acme"))]);
        let after =
            state(&[("invoice", json!({ "status": "charged" })), ("charge", json!("ch_1"))]);
        let shown = Display::default().changes(&before, &after);
        let expected = vec![
            StateChange {
                key: "charge".into(),
                change: Change::Added,
                change_text: "Added".into(),
                before: None,
                after: Some(json!("ch_1")),
                note: None,
            },
            StateChange {
                key: "invoice".into(),
                change: Change::Changed,
                change_text: "Changed".into(),
                before: Some(json!({ "status": "ready" })),
                after: Some(json!({ "status": "charged" })),
                note: None,
            },
            StateChange {
                key: "note".into(),
                change: Change::Removed,
                change_text: "Removed".into(),
                before: Some(json!("Bill Acme")),
                after: None,
                note: None,
            },
        ];
        assert_eq!(shown, expected);
    }

    #[test]
    fn a_secret_is_redacted_and_says_why() {
        let after = state(&[(
            "charge",
            json!({ "id": "ch_1", "auth": "Bearer abcdefghijklmnopqrstuvwxyz0123", "token": "tok_live_0123456789abcdef" }),
        )]);
        let shown = Display::default().changes(&BTreeMap::new(), &after);
        assert_eq!(
            shown,
            vec![StateChange {
                key: "charge".into(),
                change: Change::Added,
                change_text: "Added".into(),
                before: None,
                after: Some(json!({ "id": "ch_1", "auth": "[redacted]", "token": "[redacted]" })),
                note: Some("Hidden: looks like a secret".into()),
            }]
        );
    }

    #[test]
    fn hidden_keys_are_hidden_at_any_depth() {
        let display = Display::new(["apiKey".to_string(), "card".to_string()]);
        let channels = vec![
            display.channel("apiKey", Some(&json!("k-1"))),
            display.channel("charge", Some(&json!({ "id": "ch_1", "Card": "4242" }))),
            display.channel("receipt", None),
        ];
        assert_eq!(
            channels,
            vec![
                ChannelValue {
                    key: "apiKey".into(),
                    value: json!("[hidden]"),
                    note: Some("Hidden: this home hides this key".into()),
                },
                ChannelValue {
                    key: "charge".into(),
                    value: json!({ "id": "ch_1", "Card": "[hidden]" }),
                    note: Some("Hidden: this home hides this key".into()),
                },
                ChannelValue { key: "receipt".into(), value: Value::Null, note: None },
            ]
        );
    }

    #[test]
    fn a_large_value_is_capped() {
        let big = json!({ "lines": "x".repeat(VALUE_CAP * 2) });
        let shown = Display::default().channel("invoice", Some(&big));
        let Value::String(text) = &shown.value else { panic!("a capped value is text") };
        assert_eq!(
            (text.chars().count(), shown.note.as_deref()),
            (VALUE_CAP + 1, Some(words::TOO_LARGE))
        );
        assert!(text.ends_with('…'));
    }

    fn values() -> impl Strategy<Value = Value> {
        prop_oneof![
            Just(Value::Null),
            any::<i64>().prop_map(Value::from),
            "[a-z]{0,6}".prop_map(Value::from),
            proptest::collection::btree_map("[a-c]", any::<u8>(), 0..3).prop_map(|map| json!(map)),
        ]
    }

    fn states() -> impl Strategy<Value = BTreeMap<String, Value>> {
        proptest::collection::btree_map("[a-f]", values(), 0..6)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Spec 3.3 §12: applying "What changed" to the state before yields the state after,
        /// and every key it names did change.
        #[test]
        fn applying_the_changes_to_before_yields_after(before in states(), after in states()) {
            let changes = diff(&before, &after);
            prop_assert_eq!(apply(&before, &changes), after.clone());
            for change in &changes {
                prop_assert_ne!(before.get(&change.key), after.get(&change.key));
            }
        }
    }
}
