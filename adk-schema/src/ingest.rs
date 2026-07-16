use crate::document::SchemaMetrics;
use crate::error::{LimitKind, ReferenceRejection, Result, SchemaError};
use crate::policy::IngestionPolicy;
use crate::references::{PointerError, parse_local_ref, resolve_local_pointer};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub(crate) struct ReferenceEdge {
    pub(crate) schema_source: String,
    pub(crate) keyword_pointer: String,
    pub(crate) target: String,
    pub(crate) raw: String,
}

pub(crate) struct SchemaScan {
    pub(crate) references: Vec<ReferenceEdge>,
    pub(crate) schema_locations: HashSet<String>,
}

struct StructuralFrame<'a> {
    value: &'a Value,
    depth: usize,
    pointer: String,
}

pub(crate) fn scan_all_nodes_iteratively(
    root: &Value,
    policy: &IngestionPolicy,
) -> Result<SchemaMetrics> {
    let mut stack = vec![StructuralFrame { value: root, depth: 1, pointer: String::new() }];
    let mut node_count = 0;
    let mut max_depth = 0;

    while let Some(frame) = stack.pop() {
        node_count += 1;
        if node_count > policy.max_nodes {
            return Err(SchemaError::LimitExceeded {
                kind: LimitKind::NodeCount,
                limit: policy.max_nodes,
                observed: node_count,
                pointer: frame.pointer,
            });
        }
        if frame.depth > policy.max_depth {
            return Err(SchemaError::LimitExceeded {
                kind: LimitKind::NestingDepth,
                limit: policy.max_depth,
                observed: frame.depth,
                pointer: frame.pointer,
            });
        }
        if frame.depth > max_depth {
            max_depth = frame.depth;
        }

        match frame.value {
            Value::Object(map) => {
                for (key, val) in map {
                    let next_pointer = if frame.pointer.is_empty() {
                        format!("/{}", key.replace('~', "~0").replace('/', "~1"))
                    } else {
                        format!("{}/{}", frame.pointer, key.replace('~', "~0").replace('/', "~1"))
                    };
                    stack.push(StructuralFrame {
                        value: val,
                        depth: frame.depth + 1,
                        pointer: next_pointer,
                    });
                }
            }
            Value::Array(arr) => {
                for (i, val) in arr.iter().enumerate() {
                    let next_pointer = format!("{}/{}", frame.pointer, i);
                    stack.push(StructuralFrame {
                        value: val,
                        depth: frame.depth + 1,
                        pointer: next_pointer,
                    });
                }
            }
            _ => {}
        }
    }

    Ok(SchemaMetrics { depth: max_depth, node_count, reference_count: 0 })
}

struct SchemaFrame<'a> {
    schema: &'a Value,
    pointer: String,
}

pub(crate) fn scan_schema_locations_iteratively(
    root: &Value,
    policy: &IngestionPolicy,
) -> Result<SchemaScan> {
    let mut stack = vec![SchemaFrame { schema: root, pointer: String::new() }];
    let mut reference_count = 0;
    let mut references = Vec::new();
    let mut schema_locations = HashSet::new();

    while let Some(frame) = stack.pop() {
        schema_locations.insert(frame.pointer.clone());

        match frame.schema {
            Value::Bool(_) => {
                // Terminal valid schema location
            }
            Value::Object(map) => {
                for (key, val) in map {
                    let next_pointer = if frame.pointer.is_empty() {
                        format!("/{}", key.replace('~', "~0").replace('/', "~1"))
                    } else {
                        format!("{}/{}", frame.pointer, key.replace('~', "~0").replace('/', "~1"))
                    };

                    if key == "$id" && !frame.pointer.is_empty() {
                        return Err(SchemaError::UnsupportedReference {
                            pointer: next_pointer,
                            reference: key.clone(),
                            reason: ReferenceRejection::NestedId,
                        });
                    }
                    if key == "$schema" && !frame.pointer.is_empty() {
                        return Err(SchemaError::UnsupportedReference {
                            pointer: next_pointer,
                            reference: key.clone(),
                            reason: ReferenceRejection::NestedSchema,
                        });
                    }
                    if key == "$anchor" || key == "$dynamicAnchor" {
                        return Err(SchemaError::UnsupportedReference {
                            pointer: next_pointer,
                            reference: key.clone(),
                            reason: ReferenceRejection::UnsupportedAnchor,
                        });
                    }
                    if key == "$dynamicRef" {
                        return Err(SchemaError::UnsupportedReference {
                            pointer: next_pointer,
                            reference: key.clone(),
                            reason: ReferenceRejection::UnsupportedDynamicRef,
                        });
                    }
                    if key == "$ref" {
                        reference_count += 1;
                        if reference_count > policy.max_references {
                            return Err(SchemaError::LimitExceeded {
                                kind: LimitKind::ReferenceCount,
                                limit: policy.max_references,
                                observed: reference_count,
                                pointer: next_pointer,
                            });
                        }
                        let ref_str = val.as_str().ok_or_else(|| SchemaError::Parse {
                            message: "$ref value must be a string".to_string(),
                        })?;
                        let target_path = parse_local_ref(ref_str).map_err(|reason| {
                            SchemaError::UnsupportedReference {
                                pointer: next_pointer.clone(),
                                reference: ref_str.to_string(),
                                reason,
                            }
                        })?;
                        references.push(ReferenceEdge {
                            schema_source: frame.pointer.clone(),
                            keyword_pointer: next_pointer,
                            target: target_path,
                            raw: ref_str.to_string(),
                        });
                    }
                }

                // Maps of schemas
                for kw in &["$defs", "properties", "patternProperties", "dependentSchemas"] {
                    if let Some(Value::Object(submap)) = map.get(*kw) {
                        for (subkey, subval) in submap {
                            let sub_pointer = if frame.pointer.is_empty() {
                                format!(
                                    "/{}/{}",
                                    kw.replace('~', "~0").replace('/', "~1"),
                                    subkey.replace('~', "~0").replace('/', "~1")
                                )
                            } else {
                                format!(
                                    "{}/{}/{}",
                                    frame.pointer,
                                    kw.replace('~', "~0").replace('/', "~1"),
                                    subkey.replace('~', "~0").replace('/', "~1")
                                )
                            };
                            stack.push(SchemaFrame { schema: subval, pointer: sub_pointer });
                        }
                    }
                }

                // Arrays of schemas
                for kw in &["prefixItems", "allOf", "anyOf", "oneOf"] {
                    if let Some(Value::Array(arr)) = map.get(*kw) {
                        for (i, subval) in arr.iter().enumerate() {
                            let sub_pointer = if frame.pointer.is_empty() {
                                format!("/{}/{}", kw.replace('~', "~0").replace('/', "~1"), i)
                            } else {
                                format!(
                                    "{}/{}/{}",
                                    frame.pointer,
                                    kw.replace('~', "~0").replace('/', "~1"),
                                    i
                                )
                            };
                            stack.push(SchemaFrame { schema: subval, pointer: sub_pointer });
                        }
                    }
                }

                // Single schemas
                for kw in &[
                    "items",
                    "contains",
                    "additionalProperties",
                    "unevaluatedItems",
                    "unevaluatedProperties",
                    "propertyNames",
                    "not",
                    "if",
                    "then",
                    "else",
                    "contentSchema",
                ] {
                    if let Some(subval) = map.get(*kw) {
                        let sub_pointer = if frame.pointer.is_empty() {
                            format!("/{}", kw.replace('~', "~0").replace('/', "~1"))
                        } else {
                            format!(
                                "{}/{}",
                                frame.pointer,
                                kw.replace('~', "~0").replace('/', "~1")
                            )
                        };
                        stack.push(SchemaFrame { schema: subval, pointer: sub_pointer });
                    }
                }
            }
            _ => {}
        }
    }

    Ok(SchemaScan { references, schema_locations })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VisitState {
    Unvisited,
    Visiting,
    Visited,
}

pub(crate) fn validate_reference_graph(
    root: &Value,
    references: &[ReferenceEdge],
    schema_locations: &HashSet<String>,
    _policy: &IngestionPolicy,
) -> Result<()> {
    // 1. Verify all targets exist, are syntactically valid pointers, point to known schema locations, and resolve to object or boolean schemas.
    for edge in references {
        let resolved = resolve_local_pointer(root, &edge.target).map_err(|err| match err {
            PointerError::Syntax => SchemaError::UnsupportedReference {
                pointer: edge.keyword_pointer.clone(),
                reference: edge.raw.clone(),
                reason: ReferenceRejection::MalformedPointer,
            },
            PointerError::Unresolved => SchemaError::MissingReference {
                pointer: edge.keyword_pointer.clone(),
                reference: edge.raw.clone(),
            },
        })?;

        // Target pointer must be in schema_locations
        if !schema_locations.contains(&edge.target) {
            return Err(SchemaError::UnsupportedReference {
                pointer: edge.keyword_pointer.clone(),
                reference: edge.raw.clone(),
                reason: ReferenceRejection::InvalidSchemaTarget,
            });
        }

        match resolved {
            Value::Object(_) | Value::Bool(_) => {}
            _ => {
                return Err(SchemaError::UnsupportedReference {
                    pointer: edge.keyword_pointer.clone(),
                    reference: edge.raw.clone(),
                    reason: ReferenceRejection::InvalidSchemaTarget,
                });
            }
        }
    }

    // 2. Cycle detection using three-state DFS with exact active path reconstruction
    let mut states: HashMap<String, VisitState> = HashMap::new();
    for edge in references {
        states.insert(edge.target.clone(), VisitState::Unvisited);
        states.insert(edge.schema_source.clone(), VisitState::Unvisited);
    }
    states.insert(String::new(), VisitState::Unvisited);

    let keys: Vec<String> = states.keys().cloned().collect();

    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for node in &keys {
        let mut targets = Vec::new();
        for edge in references {
            let is_child = if node.is_empty() {
                true
            } else {
                edge.schema_source == *node || edge.schema_source.starts_with(&format!("{}/", node))
            };
            if is_child {
                targets.push(edge.target.clone());
            }
        }
        adj.insert(node.clone(), targets);
    }

    let mut parent_map: HashMap<String, String> = HashMap::new();

    for node in keys {
        if states.get(&node) == Some(&VisitState::Unvisited) {
            let mut stack: Vec<(String, Option<String>, bool)> = vec![(node.clone(), None, false)];
            while let Some((curr, parent_opt, is_backtrack)) = stack.pop() {
                if is_backtrack {
                    states.insert(curr, VisitState::Visited);
                } else {
                    match states.get(&curr) {
                        Some(&VisitState::Visiting) => {
                            let mut cycle = vec![curr.clone()];
                            let mut temp = parent_opt.clone();
                            while let Some(ref t) = temp {
                                if t == &curr {
                                    break;
                                }
                                cycle.push(t.clone());
                                temp = parent_map.get(t).cloned();
                            }
                            cycle.push(curr.clone());
                            cycle.reverse();
                            return Err(SchemaError::ReferenceCycle { cycle });
                        }
                        Some(&VisitState::Visited) => {}
                        _ => {
                            states.insert(curr.clone(), VisitState::Visiting);
                            stack.push((curr.clone(), parent_opt.clone(), true));

                            if let Some(ref p) = parent_opt {
                                parent_map.insert(curr.clone(), p.clone());
                            }

                            if let Some(targets) = adj.get(&curr) {
                                for target in targets {
                                    if states.get(target) != Some(&VisitState::Visited) {
                                        stack.push((target.clone(), Some(curr.clone()), false));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
