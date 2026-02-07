use serde_json::{Value, json};

/// Helper functions to create A2UI v0.9 components with correct nested structure.

/// Create a Text component
pub fn text(id: &str, text: &str, variant: Option<&str>) -> Value {
    let mut component = json!({
        "text": { "literalString": text }
    });

    if let Some(v) = variant {
        component["variant"] = json!(v);
    }

    json!({
        "id": id,
        "component": {
            "Text": component
        }
    })
}

/// Create a Column layout component
pub fn column(id: &str, children: Vec<&str>) -> Value {
    json!({
        "id": id,
        "component": {
            "Column": {
                "children": children,
                "justify": "start",
                "align": "stretch"
            }
        }
    })
}

/// Create a Row layout component
pub fn row(id: &str, children: Vec<&str>) -> Value {
    json!({
        "id": id,
        "component": {
            "Row": {
                "children": children,
                "justify": "start",
                "align": "center"
            }
        }
    })
}

/// Create a Button component
pub fn button(id: &str, child_text_id: &str, action_name: &str) -> Value {
    json!({
        "id": id,
        "component": {
            "Button": {
                "child": child_text_id,
                "action": {
                    "event": {
                        "name": action_name
                    }
                }
            }
        }
    })
}

/// Create an Image component
pub fn image(id: &str, url: &str) -> Value {
    json!({
        "id": id,
        "component": {
            "Image": {
                "url": { "literalString": url }
            }
        }
    })
}

/// Create a Divider component
pub fn divider(id: &str, axis: &str) -> Value {
    json!({
        "id": id,
        "component": {
            "Divider": {
                "axis": axis
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_component() {
        let comp = text("title", "Hello World", Some("h1"));
        assert_eq!(comp["id"], "title");
        assert_eq!(comp["component"]["Text"]["text"]["literalString"], "Hello World");
        assert_eq!(comp["component"]["Text"]["variant"], "h1");
    }

    #[test]
    fn test_column_component() {
        let comp = column("root", vec!["child1", "child2"]);
        assert_eq!(comp["id"], "root");
        assert_eq!(comp["component"]["Column"]["children"][0], "child1");
        assert_eq!(comp["component"]["Column"]["children"][1], "child2");
    }

    #[test]
    fn test_button_component() {
        let comp = button("btn1", "btn_text", "submit");
        assert_eq!(comp["id"], "btn1");
        assert_eq!(comp["component"]["Button"]["child"], "btn_text");
        assert_eq!(comp["component"]["Button"]["action"]["event"]["name"], "submit");
    }
}
