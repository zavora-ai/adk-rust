use serde_json::Value;
use std::collections::HashMap;

pub trait State: Send + Sync {
    fn get(&self, key: &str) -> Option<Value>;
    fn set(&mut self, key: String, value: Value);
    fn all(&self) -> HashMap<String, Value>;
}

pub trait ReadonlyState: Send + Sync {
    fn get(&self, key: &str) -> Option<Value>;
    fn all(&self) -> HashMap<String, Value>;
}
