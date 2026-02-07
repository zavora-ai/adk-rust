mod render_alert;
mod render_card;
mod render_chart;
mod render_confirm;
mod render_form;
mod render_kit;
mod render_layout;
mod render_modal;
mod render_page;
mod render_progress;
mod render_screen;
mod render_table;
mod render_toast;

pub use render_alert::RenderAlertTool;
pub use render_card::RenderCardTool;
pub use render_chart::RenderChartTool;
pub use render_confirm::RenderConfirmTool;
pub use render_form::RenderFormTool;
pub use render_kit::RenderKitTool;
pub use render_layout::RenderLayoutTool;
pub use render_modal::RenderModalTool;
pub use render_page::RenderPageTool;
pub use render_progress::RenderProgressTool;
pub use render_screen::RenderScreenTool;
pub use render_table::RenderTableTool;
pub use render_toast::RenderToastTool;

use schemars::{JsonSchema, r#gen::SchemaSettings};
use serde::Serialize;
use serde_json::Value;

/// Generate a Gemini-compatible schema (no $schema, $ref, or definitions)
pub(crate) fn generate_gemini_schema<T>() -> Value
where
    T: JsonSchema + Serialize,
{
    let settings = SchemaSettings::openapi3().with(|s| {
        s.inline_subschemas = true;
        s.meta_schema = None;
    });
    let generator = schemars::r#gen::SchemaGenerator::new(settings);
    let mut schema = generator.into_root_schema_for::<T>();
    schema.schema.metadata().title = None;

    // Convert to Value and clean up any remaining problematic fields
    let mut value = serde_json::to_value(schema.schema).unwrap();
    clean_schema(&mut value);
    value
}

/// Remove fields that Gemini doesn't support
fn clean_schema(value: &mut Value) {
    if let Value::Object(map) = value {
        // Remove problematic fields
        map.remove("$schema");
        map.remove("definitions");
        map.remove("$ref");
        map.remove("additionalProperties");

        // Recursively clean nested objects
        for (_, v) in map.iter_mut() {
            clean_schema(v);
        }
    } else if let Value::Array(arr) = value {
        for v in arr.iter_mut() {
            clean_schema(v);
        }
    }
}
