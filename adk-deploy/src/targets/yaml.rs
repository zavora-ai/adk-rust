//! A minimal YAML emitter for generated manifests.
//!
//! Keys keep insertion order, so generated files read in the order Kubernetes and Compose
//! documentation use, and the output does not depend on `serde_json` feature unification.

use std::fmt::Write;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Yaml {
    Map(Vec<(String, Yaml)>),
    Seq(Vec<Yaml>),
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl Yaml {
    pub(crate) fn map() -> Self {
        Self::Map(Vec::new())
    }

    /// Appends `key: value` to a map; a no-op on other variants.
    pub(crate) fn with(mut self, key: &str, value: impl Into<Yaml>) -> Self {
        if let Self::Map(entries) = &mut self {
            entries.push((key.to_string(), value.into()));
        }
        self
    }

    /// Appends `key: value` when `value` is present.
    pub(crate) fn with_opt(self, key: &str, value: Option<impl Into<Yaml>>) -> Self {
        match value {
            Some(value) => self.with(key, value),
            None => self,
        }
    }

    pub(crate) fn seq<T: Into<Yaml>>(items: impl IntoIterator<Item = T>) -> Self {
        Self::Seq(items.into_iter().map(Into::into).collect())
    }

    pub(crate) fn is_empty(&self) -> bool {
        match self {
            Self::Map(entries) => entries.is_empty(),
            Self::Seq(items) => items.is_empty(),
            Self::Str(_) | Self::Int(_) | Self::Float(_) | Self::Bool(_) => false,
        }
    }

    /// Renders one YAML document.
    pub(crate) fn render(&self) -> String {
        let mut out = String::new();
        match self {
            Self::Map(_) | Self::Seq(_) if !self.is_empty() => self.block(&mut out, 0),
            _ => {
                out.push_str(&self.inline());
                out.push('\n');
            }
        }
        out
    }

    fn block(&self, out: &mut String, indent: usize) {
        let pad = " ".repeat(indent);
        match self {
            Self::Map(entries) => {
                for (key, value) in entries {
                    let key = scalar(key);
                    if value.is_block() {
                        let _ = writeln!(out, "{pad}{key}:");
                        let child = if matches!(value, Self::Seq(_)) { indent } else { indent + 2 };
                        value.block(out, child);
                    } else {
                        let _ = writeln!(out, "{pad}{key}: {}", value.inline());
                    }
                }
            }
            Self::Seq(items) => {
                for item in items {
                    if item.is_block() {
                        // The first line of a nested block sits after "- ".
                        let mut nested = String::new();
                        item.block(&mut nested, indent + 2);
                        let nested =
                            nested.strip_prefix(&" ".repeat(indent + 2)).unwrap_or(&nested);
                        let _ = write!(out, "{pad}- {nested}");
                    } else {
                        let _ = writeln!(out, "{pad}- {}", item.inline());
                    }
                }
            }
            Self::Str(_) | Self::Int(_) | Self::Float(_) | Self::Bool(_) => {
                let _ = writeln!(out, "{pad}{}", self.inline());
            }
        }
    }

    fn is_block(&self) -> bool {
        matches!(self, Self::Map(_) | Self::Seq(_)) && !self.is_empty()
    }

    fn inline(&self) -> String {
        match self {
            Self::Map(_) => "{}".to_string(),
            Self::Seq(_) => "[]".to_string(),
            Self::Str(value) => scalar(value),
            Self::Int(value) => value.to_string(),
            Self::Float(value) => value.to_string(),
            Self::Bool(value) => value.to_string(),
        }
    }
}

/// A plain scalar when YAML reads it back as the same string, a JSON-quoted one otherwise.
fn scalar(value: &str) -> String {
    let plain = !value.is_empty()
        && value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-'))
        // A leading digit can read back as a number, a date, or a hex or octal integer.
        && !value.starts_with(|c: char| c == '-' || c == '.' || c.is_ascii_digit())
        && !matches!(
            value.to_ascii_lowercase().as_str(),
            "true" | "false" | "yes" | "no" | "on" | "off" | "null" | "y" | "n" | "nan" | "inf"
        );
    if plain { value.to_string() } else { serde_json::Value::String(value.to_string()).to_string() }
}

impl From<&str> for Yaml {
    fn from(value: &str) -> Self {
        Self::Str(value.to_string())
    }
}

impl From<String> for Yaml {
    fn from(value: String) -> Self {
        Self::Str(value)
    }
}

impl From<&String> for Yaml {
    fn from(value: &String) -> Self {
        Self::Str(value.clone())
    }
}

impl From<u16> for Yaml {
    fn from(value: u16) -> Self {
        Self::Int(i64::from(value))
    }
}

impl From<u32> for Yaml {
    fn from(value: u32) -> Self {
        Self::Int(i64::from(value))
    }
}

impl From<u64> for Yaml {
    fn from(value: u64) -> Self {
        Self::Int(i64::try_from(value).unwrap_or(i64::MAX))
    }
}

impl From<u8> for Yaml {
    fn from(value: u8) -> Self {
        Self::Int(i64::from(value))
    }
}

impl From<f64> for Yaml {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl From<bool> for Yaml {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_nested_maps_and_sequences() {
        let doc = Yaml::map()
            .with("apiVersion", "apps/v1")
            .with("ports", Yaml::seq(["127.0.0.1:8080:8080"]))
            .with(
                "env",
                Yaml::seq([
                    Yaml::map().with("name", "PORT").with("value", "8080"),
                    Yaml::map().with("name", "MODE").with("value", "on"),
                ]),
            )
            .with("spec", Yaml::map().with("replicas", 2u32).with("paused", false))
            .with("empty", Yaml::map())
            .with("path", "/api/health")
            .with("dash", "-x")
            .with("memory", "512Mi");
        assert_eq!(
            doc.render(),
            "apiVersion: apps/v1\n\
             ports:\n\
             - \"127.0.0.1:8080:8080\"\n\
             env:\n\
             - name: PORT\n  value: \"8080\"\n\
             - name: MODE\n  value: \"on\"\n\
             spec:\n  replicas: 2\n  paused: false\n\
             empty: {}\n\
             path: /api/health\n\
             dash: \"-x\"\n\
             memory: \"512Mi\"\n"
        );
    }

    #[test]
    fn sequences_nest_inside_sequence_items() {
        let doc = Yaml::map().with(
            "containers",
            Yaml::seq([Yaml::map()
                .with("name", "agent")
                .with("args", Yaml::seq(["serve", "--port", "8080"]))]),
        );
        assert_eq!(
            doc.render(),
            "containers:\n- name: agent\n  args:\n  - serve\n  - \"--port\"\n  - \"8080\"\n"
        );
    }
}
