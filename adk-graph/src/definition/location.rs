//! Where each value of a YAML workflow sits, so a validation issue names its line and column.

use std::collections::HashMap;

use serde::Deserialize;
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_saphyr::Spanned;

/// A 1-based line and column in a workflow file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceLocation {
    /// The line, from 1.
    pub line: u32,
    /// The column, from 1.
    pub column: u32,
}

impl std::fmt::Display for SourceLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// The location of every value of a YAML document, keyed by its path (`edges[2].to`).
#[derive(Debug, Default)]
pub(crate) struct LocationIndex {
    paths: HashMap<String, SourceLocation>,
}

impl LocationIndex {
    /// Indexes `yaml`. A document that does not parse yields an empty index; the typed parse
    /// reports why.
    pub(crate) fn of_yaml(yaml: &str) -> Self {
        let mut index = Self::default();
        if let Ok(root) = serde_saphyr::from_str::<Spanned<Tree>>(yaml) {
            index.add(String::new(), &root);
        }
        index
    }

    fn add(&mut self, path: String, node: &Spanned<Tree>) {
        let location = &node.referenced;
        if location.line() > 0 {
            self.paths.insert(
                path.clone(),
                SourceLocation { line: location.line() as u32, column: location.column() as u32 },
            );
        }
        match &node.value {
            Tree::Map(entries) => {
                for (key, value) in entries {
                    let child = if path.is_empty() { key.clone() } else { format!("{path}.{key}") };
                    self.add(child, value);
                }
            }
            Tree::Seq(items) => {
                for (index, item) in items.iter().enumerate() {
                    self.add(format!("{path}[{index}]"), item);
                }
            }
            Tree::Scalar => {}
        }
    }

    /// The location of `path`, or of its nearest indexed ancestor.
    pub(crate) fn locate(&self, path: &str) -> Option<SourceLocation> {
        let mut path = path;
        loop {
            if let Some(location) = self.paths.get(path) {
                return Some(*location);
            }
            let cut = path.rfind(['.', '['])?;
            path = &path[..cut];
        }
    }
}

/// A YAML value's shape, with the location of each child.
enum Tree {
    Map(Vec<(String, Spanned<Tree>)>),
    Seq(Vec<Spanned<Tree>>),
    Scalar,
}

impl<'de> Deserialize<'de> for Tree {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(TreeVisitor)
    }
}

struct TreeVisitor;

impl<'de> Visitor<'de> for TreeVisitor {
    type Value = Tree;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("any YAML value")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Tree, A::Error> {
        let mut entries = Vec::new();
        while let Some(key) = map.next_key::<Key>()? {
            entries.push((key.0, map.next_value::<Spanned<Tree>>()?));
        }
        Ok(Tree::Map(entries))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Tree, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element::<Spanned<Tree>>()? {
            items.push(item);
        }
        Ok(Tree::Seq(items))
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Tree, E> {
        Ok(Tree::Scalar)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Tree, E> {
        Ok(Tree::Scalar)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Tree, E> {
        Ok(Tree::Scalar)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Tree, E> {
        Ok(Tree::Scalar)
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Tree, E> {
        Ok(Tree::Scalar)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Tree, E> {
        Ok(Tree::Scalar)
    }

    fn visit_none<E: de::Error>(self) -> Result<Tree, E> {
        Ok(Tree::Scalar)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Tree, D::Error> {
        deserializer.deserialize_any(TreeVisitor)
    }
}

/// A mapping key as text, whatever scalar it is written as.
struct Key(String);

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeyVisitor;

        impl<'de> Visitor<'de> for KeyVisitor {
            type Value = Key;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a mapping key")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Key, E> {
                Ok(Key(value.to_string()))
            }

            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Key, E> {
                Ok(Key(value.to_string()))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Key, E> {
                Ok(Key(value.to_string()))
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Key, E> {
                Ok(Key(value.to_string()))
            }

            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Key, E> {
                Ok(Key(value.to_string()))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Key, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Key(String::new()))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Key, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Key(String::new()))
            }
        }

        deserializer.deserialize_any(KeyVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_value_is_located_by_its_path() {
        let index = LocationIndex::of_yaml(
            "name: demo\nnodes:\n  a:\n    set: { x: 1 }\nedges:\n  - [START, a]\n  - from: a\n    to: END\n",
        );
        assert_eq!(index.locate("name"), Some(SourceLocation { line: 1, column: 7 }));
        assert_eq!(index.locate("nodes.a.set.x"), Some(SourceLocation { line: 4, column: 15 }));
        assert_eq!(index.locate("edges[0][1]"), Some(SourceLocation { line: 6, column: 13 }));
        assert_eq!(index.locate("edges[1].to"), Some(SourceLocation { line: 8, column: 9 }));
        // A missing key is reported at its nearest ancestor.
        assert_eq!(index.locate("edges[1].routes[0]"), index.locate("edges[1]"));
    }
}
