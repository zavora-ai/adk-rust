//////////////////////////////////////////// JsonSchema ////////////////////////////////////////////

/// Implement JsonSchema to derive the schema for GenerateRequest automatically.
pub trait JsonSchema {
    /// Return the json_schema.  Does not depend on an object.
    fn json_schema() -> serde_json::Value;
}

impl JsonSchema for bool {
    fn json_schema() -> serde_json::Value {
        serde_json::json! {{ "type": "boolean" }}
    }
}

impl JsonSchema for i8 {
    fn json_schema() -> serde_json::Value {
        serde_json::json! {{ "type": "integer" }}
    }
}

impl JsonSchema for i16 {
    fn json_schema() -> serde_json::Value {
        serde_json::json! {{ "type": "integer" }}
    }
}

impl JsonSchema for i32 {
    fn json_schema() -> serde_json::Value {
        serde_json::json! {{ "type": "integer" }}
    }
}

impl JsonSchema for i64 {
    fn json_schema() -> serde_json::Value {
        serde_json::json! {{ "type": "integer" }}
    }
}

impl JsonSchema for u8 {
    fn json_schema() -> serde_json::Value {
        serde_json::json! {{ "type": "integer" }}
    }
}

impl JsonSchema for u16 {
    fn json_schema() -> serde_json::Value {
        serde_json::json! {{ "type": "integer" }}
    }
}

impl JsonSchema for u32 {
    fn json_schema() -> serde_json::Value {
        serde_json::json! {{ "type": "integer" }}
    }
}

impl JsonSchema for u64 {
    fn json_schema() -> serde_json::Value {
        serde_json::json! {{ "type": "integer" }}
    }
}

impl JsonSchema for f32 {
    fn json_schema() -> serde_json::Value {
        serde_json::json! {{ "type": "number" }}
    }
}

impl JsonSchema for f64 {
    fn json_schema() -> serde_json::Value {
        serde_json::json! {{ "type": "number" }}
    }
}

impl JsonSchema for String {
    fn json_schema() -> serde_json::Value {
        serde_json::json! {{ "type": "string" }}
    }
}

impl<T: JsonSchema> JsonSchema for Option<T> {
    fn json_schema() -> serde_json::Value {
        let mut res = <T as JsonSchema>::json_schema();
        res["nullable"] = true.into();
        res
    }
}

impl<T: JsonSchema> JsonSchema for Vec<T> {
    fn json_schema() -> serde_json::Value {
        serde_json::json! {{ "type": "array", "items": <T as JsonSchema>::json_schema() }}
    }
}
