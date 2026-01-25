pub mod messages;
pub mod encoding;
pub mod ids;
pub mod bindings;
pub mod validator;
pub mod events;
pub mod data_model;
pub mod components;
pub mod prompts;

pub use messages::{
    A2uiMessage,
    CreateSurface,
    CreateSurfaceMessage,
    DeleteSurface,
    DeleteSurfaceMessage,
    UpdateComponents,
    UpdateComponentsMessage,
    UpdateDataModel,
    UpdateDataModelMessage,
};
pub use encoding::{encode_jsonl, encode_jsonl_bytes, encode_message_line};
pub use ids::{stable_child_id, stable_id, stable_indexed_id};
pub use bindings::DynamicString;
pub use validator::{A2uiSchemaVersion, A2uiValidator, A2uiValidationError};
pub use events::{A2uiActionEvent, A2uiActionMetadata, UiEventMapper};
pub use data_model::{DataModelUpdate, DataModelValue, UpdateDataModelBuilder};
pub use components::{text, column, row, button, image, divider};
pub use prompts::A2UI_AGENT_PROMPT;
