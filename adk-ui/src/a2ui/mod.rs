pub mod bindings;
pub mod components;
pub mod data_model;
pub mod encoding;
pub mod events;
pub mod ids;
pub mod messages;
pub mod prompts;
pub mod validator;

pub use bindings::DynamicString;
pub use components::{button, column, divider, image, row, text};
pub use data_model::{DataModelUpdate, DataModelValue, UpdateDataModelBuilder};
pub use encoding::{encode_jsonl, encode_jsonl_bytes, encode_message_line};
pub use events::{A2uiActionEvent, A2uiActionMetadata, UiEventMapper};
pub use ids::{stable_child_id, stable_id, stable_indexed_id};
pub use messages::{
    A2uiMessage, CreateSurface, CreateSurfaceMessage, DeleteSurface, DeleteSurfaceMessage,
    UpdateComponents, UpdateComponentsMessage, UpdateDataModel, UpdateDataModelMessage,
};
pub use prompts::A2UI_AGENT_PROMPT;
pub use validator::{A2uiSchemaVersion, A2uiValidationError, A2uiValidator};
