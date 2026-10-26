//! Agent-drawn UI (spec 3.3 §6.1 `genui`, §7.4 rule 5): forms, cards, tables and screens an
//! agent draws in a person's threads, which never contain approval controls.
//!
//! | Part | What |
//! |---|---|
//! | [`A2uiMessage`] and the rest of the A2UI v0.9.1 model | The wire format an agent draws in |
//! | [`Catalog::forms`] | The `forms` 1.0 catalog: the A2UI basic catalog without images, icons, media and modals, embedded |
//! | [`Validator`] | Rule 5: the catalog, no secrets, no links, declared actions only, and no decision's words in any string a person reads, resolved on the server |
//! | [`DrawToolset`] | `render_form`, `render_card`, `render_table`, `render_screen`, `patch_surface`, `delete_surface` |
//!
//! A form's Send never runs a tool: it asks for a call, which the drawing agent holds for the
//! person's decision ([`adk_core::RequestedCall`]).

mod a2ui;
mod catalog;
mod draw;
mod validate;

pub use a2ui::{
    A2UI_MIME_TYPE, A2UI_VERSION, A2uiBody, A2uiCapabilities, A2uiClientBody, A2uiClientMessage,
    A2uiError, A2uiMessage, CapabilitySet, ClientAction, ClientError, CreateSurface, DataBinding,
    DataModel, DeleteSurface, DynamicString, FunctionCall, UpdateComponents, UpdateDataModel,
    decode_jsonl, encode_jsonl, stable_id,
};
pub use catalog::{Catalog, FORMS_CATALOG_ID};
pub use draw::{DRAWN_STATE_KEY, DrawToolset, DrawnEnvelope, DrawnRecord};
pub use validate::{
    ActionArg, ComponentRef, DrawRefusal, DrawnAction, DrawnKind, FieldArg, MAX_ACTIONS,
    MAX_COMPONENTS, MAX_DEPTH, MAX_DRAWING_BYTES, MAX_LIST_ITEMS, MAX_SHOWN, MAX_TEXT_CHARS,
    RESERVED_LABELS, RESERVED_PHRASES, RefusalKind, ResolvedText, SurfaceDraft, ValidatedSurface,
    Validator, reserved,
};
