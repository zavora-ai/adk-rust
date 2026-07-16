use crate::document::JsonSchemaDialect;
use crate::error::{LimitKind, Result, SchemaError};
use serde_json::Value;
use std::io::{Error as IoError, ErrorKind, Result as IoResult, Write};

struct LimitedWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl LimitedWriter {
    fn new(limit: usize) -> Self {
        Self { bytes: Vec::new(), limit }
    }
    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for LimitedWriter {
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        if self.bytes.len() + buf.len() > self.limit {
            return Err(IoError::new(ErrorKind::OutOfMemory, "limit exceeded"));
        }
        self.bytes.write(buf)
    }
    fn flush(&mut self) -> IoResult<()> {
        self.bytes.flush()
    }
}

pub(crate) fn serialize_bounded(value: &Value, limit: usize) -> Result<Vec<u8>> {
    let mut writer = LimitedWriter::new(limit);
    serde_json::to_writer(&mut writer, value).map_err(|e| {
        if e.is_io() {
            SchemaError::LimitExceeded {
                kind: LimitKind::CanonicalBytes,
                limit,
                observed: limit + 1,
                pointer: String::new(),
            }
        } else {
            SchemaError::Serialization(e.to_string())
        }
    })?;
    Ok(writer.into_inner())
}

pub(crate) fn canonicalize(value: Value, expected: JsonSchemaDialect) -> Result<Value> {
    let mut val = value;
    if let Some(obj) = val.as_object_mut()
        && let Some(schema_val) = obj.get("$schema")
    {
        let schema_str = schema_val.as_str().ok_or_else(|| SchemaError::Parse {
            message: "$schema must be a string".to_string(),
        })?;
        let expected_uri = match expected {
            JsonSchemaDialect::Draft202012 => "https://json-schema.org/draft/2020-12/schema",
        };
        if schema_str != expected_uri {
            return Err(SchemaError::DialectMismatch {
                declared: schema_str.to_string(),
                expected,
            });
        }
        obj.remove("$schema");
    }
    Ok(sort_value(val))
}

fn sort_value(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut sorted = serde_json::Map::with_capacity(map.len());
            let mut entries: Vec<_> = map.into_iter().collect();
            entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            for (k, val) in entries {
                sorted.insert(k, sort_value(val));
            }
            Value::Object(sorted)
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(sort_value).collect()),
        _ => v,
    }
}
