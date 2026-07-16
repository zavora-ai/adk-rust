use adk_schema::{
    IngestionPolicy, InputSchema, LimitKind, OutputSchema, ReferenceRejection, SchemaError,
};
use serde_json::json;

#[test]
fn test_empty_string_key_resolution() {
    let schema = json!({
        "properties": {
            "foo": {
                "$ref": "#/properties/"
            },
            "": { "type": "string" }
        }
    });
    let policy = IngestionPolicy::default();
    let doc = InputSchema::from_value(schema, &policy).unwrap();
    assert_eq!(doc.metrics().reference_count, 1);
}

#[test]
fn test_utf8_percent_encoding() {
    let schema = json!({
        "properties": {
            "foo": {
                "$ref": "#/$defs/%C3%A9"
            }
        },
        "$defs": {
            "é": { "type": "string" }
        }
    });
    let policy = IngestionPolicy::default();
    let doc = InputSchema::from_value(schema, &policy).unwrap();
    assert_eq!(doc.metrics().reference_count, 1);
}

#[test]
fn test_invalid_percent_sequences() {
    let schema = json!({
        "properties": {
            "foo": {
                "$ref": "#/$defs/%G1"
            }
        }
    });
    let policy = IngestionPolicy::default();
    let res = InputSchema::from_value(schema, &policy);
    assert!(matches!(
        res,
        Err(SchemaError::UnsupportedReference { reason: ReferenceRejection::MalformedPointer, .. })
    ));
}

#[test]
fn test_invalid_tilde_escapes() {
    let schema = json!({
        "properties": {
            "foo": {
                "$ref": "#/$defs/~2"
            }
        }
    });
    let policy = IngestionPolicy::default();
    let res = InputSchema::from_value(schema, &policy);
    assert!(matches!(
        res,
        Err(SchemaError::UnsupportedReference { reason: ReferenceRejection::MalformedPointer, .. })
    ));
}

#[test]
fn test_missing_vs_malformed_pointers() {
    let policy = IngestionPolicy::default();
    let missing_schema = json!({
        "properties": {
            "foo": {
                "$ref": "#/$defs/nonexistent"
            }
        }
    });
    let res1 = InputSchema::from_value(missing_schema, &policy);
    assert!(matches!(res1, Err(SchemaError::MissingReference { .. })));

    let malformed_schema = json!({
        "properties": {
            "foo": {
                "$ref": "#/$defs/~"
            }
        }
    });
    let res2 = InputSchema::from_value(malformed_schema, &policy);
    assert!(matches!(
        res2,
        Err(SchemaError::UnsupportedReference { reason: ReferenceRejection::MalformedPointer, .. })
    ));
}

#[test]
fn test_root_self_reference_fails() {
    let schema = json!({
        "$ref": "#"
    });
    let policy = IngestionPolicy::default();
    let res = InputSchema::from_value(schema, &policy);
    assert!(matches!(res, Err(SchemaError::ReferenceCycle { .. })));
}

#[test]
fn test_multi_node_cycle_fails() {
    let schema = json!({
        "$defs": {
            "a": {
                "$ref": "#/$defs/b"
            },
            "b": {
                "$ref": "#/$defs/a"
            }
        }
    });
    let policy = IngestionPolicy::default();
    let res = InputSchema::from_value(schema, &policy);
    assert!(matches!(res, Err(SchemaError::ReferenceCycle { .. })));
}

#[test]
fn test_repeated_acyclic_references_metrics() {
    let schema = json!({
        "properties": {
            "a": {
                "$ref": "#/$defs/shared"
            },
            "b": {
                "$ref": "#/$defs/shared"
            }
        },
        "$defs": {
            "shared": { "type": "string" }
        }
    });
    let policy = IngestionPolicy::default();
    let doc = InputSchema::from_value(schema, &policy).unwrap();
    assert_eq!(doc.metrics().node_count, 9);
}

#[test]
fn test_structural_nodes_counted_once() {
    let schema = json!({
        "a": 1,
        "b": [2, 3]
    });
    let policy = IngestionPolicy::default();
    let doc = InputSchema::from_value(schema, &policy).unwrap();
    assert_eq!(doc.metrics().node_count, 5);
}

#[test]
fn test_depth_without_recursive_traversal() {
    let schema = json!({
        "a": {
            "b": {
                "c": 1
            }
        }
    });
    let policy = IngestionPolicy::default();
    let doc = InputSchema::from_value(schema, &policy).unwrap();
    assert_eq!(doc.metrics().depth, 4);
}

#[test]
fn test_source_input_oversized_rejected_before_parsing() {
    let bytes = vec![b'{'; 100];
    let policy = IngestionPolicy { max_source_bytes: 50, ..IngestionPolicy::default() };
    let res = InputSchema::from_json_slice(&bytes, &policy);
    assert!(matches!(res, Err(SchemaError::LimitExceeded { kind: LimitKind::SourceBytes, .. })));
}

#[test]
fn test_bounded_serialization_limit() {
    let schema = json!({
        "a": "a very long string that will cross the canonical byte limit easily"
    });
    let policy = IngestionPolicy { max_canonical_bytes: 20, ..IngestionPolicy::default() };
    let res = InputSchema::from_value(schema, &policy);
    assert!(matches!(res, Err(SchemaError::LimitExceeded { kind: LimitKind::CanonicalBytes, .. })));
}

#[test]
fn test_no_external_http_or_file_retrieval() {
    let schema = json!({
        "properties": {
            "foo": {
                "$ref": "http://example.com/schema.json"
            }
        }
    });
    let policy = IngestionPolicy::default();
    let res = InputSchema::from_value(schema, &policy);
    assert!(matches!(
        res,
        Err(SchemaError::UnsupportedReference {
            reason: ReferenceRejection::NonLocalReference,
            ..
        })
    ));
}

#[test]
fn test_anchors_rejected() {
    let schema = json!({
        "properties": {
            "foo": {
                "$anchor": "anchor-name"
            }
        }
    });
    let policy = IngestionPolicy::default();
    let res = InputSchema::from_value(schema, &policy);
    assert!(matches!(
        res,
        Err(SchemaError::UnsupportedReference {
            reason: ReferenceRejection::UnsupportedAnchor,
            ..
        })
    ));
}

#[test]
fn test_dynamic_ref_rejected() {
    let schema = json!({
        "properties": {
            "foo": {
                "$dynamicRef": "#/$defs/foo"
            }
        }
    });
    let policy = IngestionPolicy::default();
    let res = InputSchema::from_value(schema, &policy);
    assert!(matches!(
        res,
        Err(SchemaError::UnsupportedReference {
            reason: ReferenceRejection::UnsupportedDynamicRef,
            ..
        })
    ));
}

#[test]
fn test_canonicalization_preserves_annotations() {
    let schema = json!({
        "title": "My Schema",
        "description": "Important metadata",
        "type": "string"
    });
    let policy = IngestionPolicy::default();
    let doc = InputSchema::from_value(schema, &policy).unwrap();
    let val = doc.as_value();
    assert_eq!(val.get("title").unwrap().as_str().unwrap(), "My Schema");
    assert_eq!(val.get("description").unwrap().as_str().unwrap(), "Important metadata");
}

#[test]
fn test_equivalent_object_ordering() {
    let schema_a = json!({
        "properties": {
            "x": { "type": "integer" },
            "y": { "type": "string" }
        }
    });
    let schema_b = json!({
        "properties": {
            "y": { "type": "string" },
            "x": { "type": "integer" }
        }
    });
    let policy = IngestionPolicy::default();
    let doc_a = InputSchema::from_value(schema_a, &policy).unwrap();
    let doc_b = InputSchema::from_value(schema_b, &policy).unwrap();
    assert_eq!(doc_a.digest(), doc_b.digest());
}

#[test]
fn test_changed_array_ordering() {
    let schema_a = json!({
        "type": ["string", "integer"]
    });
    let schema_b = json!({
        "type": ["integer", "string"]
    });
    let policy = IngestionPolicy::default();
    let doc_a = InputSchema::from_value(schema_a, &policy).unwrap();
    let doc_b = InputSchema::from_value(schema_b, &policy).unwrap();
    assert_ne!(doc_a.digest(), doc_b.digest());
}

#[test]
fn test_input_and_output_different_digests() {
    let schema = json!({
        "type": "string"
    });
    let policy = IngestionPolicy::default();
    let input_doc = InputSchema::from_value(schema.clone(), &policy).unwrap();
    let output_doc = OutputSchema::from_value(schema, &policy).unwrap();
    assert_ne!(input_doc.digest(), output_doc.digest());
}

#[cfg(feature = "schemars")]
#[test]
fn test_jsonschema_without_serialize() {
    use schemars::JsonSchema;

    #[derive(JsonSchema)]
    struct Dummy {
        #[allow(dead_code)]
        val: String,
    }

    let doc = InputSchema::for_type::<Dummy>();
    assert!(doc.is_ok());
}

#[test]
fn test_nested_id_rejected() {
    let schema = json!({
        "properties": {
            "sub": {
                "$id": "http://example.com/sub.json",
                "type": "string"
            }
        }
    });
    let policy = IngestionPolicy::default();
    let res = InputSchema::from_value(schema, &policy);
    assert!(matches!(
        res,
        Err(SchemaError::UnsupportedReference { reason: ReferenceRejection::NestedId, .. })
    ));
}

#[test]
fn test_nested_schema_rejected() {
    let schema = json!({
        "properties": {
            "sub": {
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "string"
            }
        }
    });
    let policy = IngestionPolicy::default();
    let res = InputSchema::from_value(schema, &policy);
    assert!(matches!(
        res,
        Err(SchemaError::UnsupportedReference { reason: ReferenceRejection::NestedSchema, .. })
    ));
}

#[test]
fn test_ref_counts_as_structural_node() {
    let schema = json!({
        "properties": {
            "a": { "$ref": "#/$defs/s" },
            "b": { "$ref": "#/$defs/s" }
        },
        "$defs": {
            "s": { "type": "string" }
        }
    });
    let policy = IngestionPolicy::default();
    let doc = InputSchema::from_value(schema, &policy).unwrap();
    assert_eq!(doc.metrics().node_count, 9);
}

#[test]
fn test_canonical_identity_independent_of_root_schema_declaration() {
    let schema_with = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "string"
    });
    let schema_without = json!({
        "type": "string"
    });
    let policy = IngestionPolicy::default();
    let doc_with = InputSchema::from_value(schema_with, &policy).unwrap();
    let doc_without = InputSchema::from_value(schema_without, &policy).unwrap();

    assert_eq!(doc_with.digest(), doc_without.digest());
    assert_eq!(doc_with, doc_without);
}

#[test]
fn test_cycle_path_exact_traversal_reporting() {
    let schema = json!({
        "properties": {
            "a": { "$ref": "#/properties/b" },
            "b": { "$ref": "#/properties/a" }
        }
    });
    let policy = IngestionPolicy::default();
    let res = InputSchema::from_value(schema, &policy);
    if let Err(SchemaError::ReferenceCycle { cycle }) = res {
        assert!(
            cycle
                == vec![
                    "/properties/a".to_string(),
                    "/properties/b".to_string(),
                    "/properties/a".to_string()
                ]
                || cycle
                    == vec![
                        "/properties/b".to_string(),
                        "/properties/a".to_string(),
                        "/properties/b".to_string()
                    ]
        );
    } else {
        panic!("Expected ReferenceCycle error");
    }
}

#[cfg(feature = "runtime-validation")]
#[test]
fn test_invalid_schema_compilation() {
    let schema = json!({
        "type": 123
    });
    let policy = IngestionPolicy::default();
    let doc = InputSchema::from_value(schema, &policy).unwrap();
    let res = doc.compile();
    assert!(matches!(res, Err(SchemaError::InvalidSchema { .. })));
}

#[cfg(feature = "runtime-validation")]
#[test]
fn test_invalid_instance_validation() {
    let schema = json!({
        "type": "string"
    });
    let policy = IngestionPolicy::default();
    let doc = InputSchema::from_value(schema, &policy).unwrap();
    let validated = doc.compile().unwrap();
    let instance = json!(123);
    let res = validated.validate(&instance);
    assert!(matches!(res, Err(SchemaError::InvalidInstance { .. })));
}

#[test]
fn test_opaque_literal_value_handling() {
    let schema = json!({
        "const": {
            "$ref": "literal-val",
            "$id": "customer-id",
            "$schema": "application-v1"
        },
        "enum": [
            {
                "$ref": "some-ref"
            }
        ],
        "default": {
            "$schema": "default-schema"
        },
        "examples": [
            {
                "$id": "nested-id-in-example"
            }
        ],
        "myCustomAnnotation": {
            "$ref": "custom-ref"
        }
    });

    let policy = IngestionPolicy::default();
    let doc = InputSchema::from_value(schema.clone(), &policy);
    assert!(doc.is_ok(), "Ingestion must succeed for literal values containing schema keys");
    let doc = doc.unwrap();
    assert_eq!(doc.as_value()["const"]["$ref"], "literal-val");
    assert_eq!(doc.as_value()["examples"][0]["$id"], "nested-id-in-example");
    assert_eq!(
        doc.metrics().reference_count,
        0,
        "Literal properties must not count as schema references"
    );
}

#[test]
fn test_subschema_keyword_locations() {
    let schema = json!({
        "$defs": {
            "my_def": { "type": "string" }
        },
        "properties": {
            "foo": { "$ref": "#/$defs/my_def" }
        },
        "unevaluatedItems": {
            "type": "integer"
        },
        "contentSchema": {
            "type": "object"
        },
        "allOf": [
            { "type": "object" }
        ]
    });
    let policy = IngestionPolicy::default();
    let doc = InputSchema::from_value(schema, &policy).unwrap();
    assert_eq!(doc.metrics().reference_count, 1);
}

#[test]
fn test_nested_id_rejection_locations() {
    let policy = IngestionPolicy::default();

    // Map-based subschema rejection
    let schema1 = json!({
        "properties": {
            "foo": {
                "$id": "nested"
            }
        }
    });
    assert!(matches!(
        InputSchema::from_value(schema1, &policy),
        Err(SchemaError::UnsupportedReference { reason: ReferenceRejection::NestedId, .. })
    ));

    // Array-based subschema rejection
    let schema2 = json!({
        "allOf": [
            {
                "$id": "nested"
            }
        ]
    });
    assert!(matches!(
        InputSchema::from_value(schema2, &policy),
        Err(SchemaError::UnsupportedReference { reason: ReferenceRejection::NestedId, .. })
    ));

    // Single subschema rejection
    let schema3 = json!({
        "items": {
            "$id": "nested"
        }
    });
    assert!(matches!(
        InputSchema::from_value(schema3, &policy),
        Err(SchemaError::UnsupportedReference { reason: ReferenceRejection::NestedId, .. })
    ));
}

#[test]
fn test_boolean_schema_support() {
    let schema = json!({
        "properties": {
            "allowed": true,
            "forbidden": false
        }
    });
    let policy = IngestionPolicy::default();
    let doc = InputSchema::from_value(schema, &policy);
    assert!(doc.is_ok());
}

#[test]
fn test_pointer_escapes_resolutions() {
    let schema = json!({
        "properties": {
            "a/b": { "type": "string" },
            "a~b": { "type": "integer" },
            "ref_a": { "$ref": "#/properties/a~1b" },
            "ref_b": { "$ref": "#/properties/a~0b" }
        }
    });
    let policy = IngestionPolicy::default();
    let doc = InputSchema::from_value(schema, &policy).unwrap();
    assert_eq!(doc.metrics().reference_count, 2);
    // Verify original strings preserved in canonical form
    assert_eq!(doc.as_value()["properties"]["ref_a"]["$ref"], "#/properties/a~1b");
    assert_eq!(doc.as_value()["properties"]["ref_b"]["$ref"], "#/properties/a~0b");
}

#[test]
fn test_reference_targets_outcomes() {
    let policy = IngestionPolicy::default();

    // 1. Missing Reference
    let schema1 = json!({
        "properties": {
            "foo": { "$ref": "#/$defs/missing" }
        }
    });
    assert!(matches!(
        InputSchema::from_value(schema1, &policy),
        Err(SchemaError::MissingReference { .. })
    ));

    // 2. Invalid Reference Target (resolves to String scalar)
    let schema2 = json!({
        "properties": {
            "foo": { "$ref": "#/type" }
        },
        "type": "string"
    });
    assert!(matches!(
        InputSchema::from_value(schema2, &policy),
        Err(SchemaError::UnsupportedReference {
            reason: ReferenceRejection::InvalidSchemaTarget,
            ..
        })
    ));

    // 3. Object Target (accepted)
    let schema3 = json!({
        "properties": {
            "foo": { "$ref": "#/$defs/ok" }
        },
        "$defs": {
            "ok": { "type": "string" }
        }
    });
    assert!(InputSchema::from_value(schema3, &policy).is_ok());

    // 4. Boolean Target (accepted)
    let schema4 = json!({
        "properties": {
            "foo": { "$ref": "#/$defs/ok" }
        },
        "$defs": {
            "ok": true
        }
    });
    assert!(InputSchema::from_value(schema4, &policy).is_ok());
}

#[test]
fn test_pointer_syntax_vs_evaluation_errors() {
    let policy = IngestionPolicy::default();

    // Syntax: invalid percent encoding
    let s1 = json!({
        "properties": { "foo": { "$ref": "#/$defs/%G1" } }
    });
    assert!(matches!(
        InputSchema::from_value(s1, &policy),
        Err(SchemaError::UnsupportedReference { reason: ReferenceRejection::MalformedPointer, .. })
    ));

    // Syntax: invalid tilde escape
    let s2 = json!({
        "properties": { "foo": { "$ref": "#/$defs/a~2b" } }
    });
    assert!(matches!(
        InputSchema::from_value(s2, &policy),
        Err(SchemaError::UnsupportedReference { reason: ReferenceRejection::MalformedPointer, .. })
    ));

    // Unresolved: missing object key
    let s3 = json!({
        "properties": { "foo": { "$ref": "#/missing" } }
    });
    assert!(matches!(
        InputSchema::from_value(s3, &policy),
        Err(SchemaError::MissingReference { .. })
    ));

    // Unresolved: scalar descent
    let s4 = json!({
        "properties": { "foo": { "$ref": "#/type/nested" } },
        "type": "string"
    });
    assert!(matches!(
        InputSchema::from_value(s4, &policy),
        Err(SchemaError::MissingReference { .. })
    ));

    // Unresolved: array token "01" (leading zero is syntax-invalid for array but treated as unresolved pointer evaluation)
    let s5 = json!({
        "items": [
            { "type": "string" }
        ],
        "properties": { "foo": { "$ref": "#/items/01" } }
    });
    assert!(matches!(
        InputSchema::from_value(s5, &policy),
        Err(SchemaError::MissingReference { .. })
    ));

    // Unresolved: array token "-"
    let s6 = json!({
        "items": [
            { "type": "string" }
        ],
        "properties": { "foo": { "$ref": "#/items/-" } }
    });
    assert!(matches!(
        InputSchema::from_value(s6, &policy),
        Err(SchemaError::MissingReference { .. })
    ));

    // Unresolved: array index 99
    let s7 = json!({
        "items": [
            { "type": "string" }
        ],
        "properties": { "foo": { "$ref": "#/items/99" } }
    });
    assert!(matches!(
        InputSchema::from_value(s7, &policy),
        Err(SchemaError::MissingReference { .. })
    ));
}

#[test]
fn test_deep_hostile_json_rejection() {
    let mut root = json!({ "type": "object" });
    for _ in 0..150 {
        root = json!({
            "properties": {
                "next": root
            }
        });
    }

    let policy = IngestionPolicy { max_depth: 50, ..IngestionPolicy::default() };
    let res = InputSchema::from_value(root, &policy);
    assert!(matches!(res, Err(SchemaError::LimitExceeded { kind: LimitKind::NestingDepth, .. })));
}

#[test]
fn test_non_schema_location_targets_rejected() {
    let policy = IngestionPolicy::default();

    // 1. $ref pointing to object under const
    let s1 = json!({
        "const": {
            "my_object": { "type": "string" }
        },
        "properties": {
            "foo": { "$ref": "#/const/my_object" }
        }
    });
    assert!(matches!(
        InputSchema::from_value(s1, &policy),
        Err(SchemaError::UnsupportedReference {
            reason: ReferenceRejection::InvalidSchemaTarget,
            ..
        })
    ));

    // 2. $ref pointing to boolean under default
    let s2 = json!({
        "default": true,
        "properties": {
            "foo": { "$ref": "#/default" }
        }
    });
    assert!(matches!(
        InputSchema::from_value(s2, &policy),
        Err(SchemaError::UnsupportedReference {
            reason: ReferenceRejection::InvalidSchemaTarget,
            ..
        })
    ));

    // 3. $ref pointing to object under examples
    let s3 = json!({
        "examples": [
            { "type": "string" }
        ],
        "properties": {
            "foo": { "$ref": "#/examples/0" }
        }
    });
    assert!(matches!(
        InputSchema::from_value(s3, &policy),
        Err(SchemaError::UnsupportedReference {
            reason: ReferenceRejection::InvalidSchemaTarget,
            ..
        })
    ));

    // 4. $ref pointing to object under unknown annotation
    let s4 = json!({
        "myCustomAnnotation": { "type": "string" },
        "properties": {
            "foo": { "$ref": "#/myCustomAnnotation" }
        }
    });
    assert!(matches!(
        InputSchema::from_value(s4, &policy),
        Err(SchemaError::UnsupportedReference {
            reason: ReferenceRejection::InvalidSchemaTarget,
            ..
        })
    ));

    // 5. Bypass test with self-cycle under const and reference from a valid property
    let s5 = json!({
        "const": {
            "$ref": "#/const"
        },
        "properties": {
            "value": {
                "$ref": "#/const"
            }
        }
    });
    assert!(matches!(
        InputSchema::from_value(s5, &policy),
        Err(SchemaError::UnsupportedReference {
            reason: ReferenceRejection::InvalidSchemaTarget,
            ..
        })
    ));
}

#[test]
fn test_diagnostic_pointers_precision() {
    let policy = IngestionPolicy::default();

    // Nested missing reference -> pointer == "/properties/foo/$ref"
    let s_nested = json!({
        "properties": {
            "foo": {
                "$ref": "#/missing"
            }
        }
    });
    let res = InputSchema::from_value(s_nested, &policy);
    assert!(matches!(
        res,
        Err(SchemaError::MissingReference { pointer, .. }) if pointer == "/properties/foo/$ref"
    ));

    // Root missing reference -> pointer == "/$ref"
    let s_root = json!({
        "$ref": "#/missing"
    });
    let res2 = InputSchema::from_value(s_root, &policy);
    assert!(matches!(
        res2,
        Err(SchemaError::MissingReference { pointer, .. }) if pointer == "/$ref"
    ));
}
