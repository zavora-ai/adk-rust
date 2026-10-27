//! Agent-drawn UI (spec 3.3 §6.1 `genui`, §7.4 rule 5): forms, cards, tables and screens an
//! agent draws in a person's threads, which never contain approval controls.
//!
//! | Part | What | Feature |
//! |---|---|---|
//! | `A2uiMessage` and the rest of the A2UI v0.9.1 model | The wire format an agent draws in | `genui` |
//! | `Catalog::forms` | The `forms` 1.0 catalog: the A2UI basic catalog without images, icons, media and modals, embedded | `genui` |
//! | `Validator` | Rule 5: the catalog, no secrets, no links, declared actions only, and no decision's words in any string a person reads, resolved on the server | `genui` |
//! | `DrawToolset` | `render_form`, `render_card`, `render_table`, `render_screen`, `patch_surface`, `delete_surface` | `genui` |
//! | `capabilities` | What this home claims of A2UI, AG-UI and MCP Apps, for the developer panel | `genui` |
//! | `GenuiAgUi` | Drawings as AG-UI `a2ui-surface` activities, and a form's send forwarded by an AG-UI client, on adk-server's AG-UI runtime | `genui` |
//! | The thread items ([`DrawnSurface`], [`RefusedDrawing`], [`DrawnSent`]) and [`form_states`] | What every surface shows of a drawing, and of a form sent | every build |
//! | `McpAppsHost` | Apps from MCP servers in a thread, sandboxed on their own origin, which can never approve anything | `genui-apps` |
//! | [`AppView`], [`AppOpen`] and the app's requests | What every surface shows of an app, and what the page relays for it | every build |
//!
//! A form's Send never runs a tool: [`Command::SubmitDrawn`](crate::Command::SubmitDrawn)
//! asks for a call ([`adk_core::RequestedCall`]), which the drawing agent holds for the
//! person's decision inside its own kernel target ([`Source::request_call`](crate::Source::request_call)).
//! The decision's card starts "You pressed Send on a form Finance drew" and never offers
//! **Always allow** (wave 4 decision 12).
//!
//! The thread items are wire types of every `web` build, so the page is the same for every
//! server; a server built without `genui` never projects a drawing. The parts marked `genui`
//! exist only with that feature.

#[cfg(feature = "genui")]
mod a2ui;
#[cfg(feature = "genui")]
mod ag_ui;
mod app_view;
#[cfg(feature = "genui")]
mod capabilities;
mod capabilities_wire;
#[cfg(feature = "genui")]
mod catalog;
#[cfg(feature = "genui")]
mod draw;
mod drawn;
#[cfg(feature = "genui")]
mod hints;
#[cfg(feature = "genui-apps")]
mod mcp_apps;
mod status;
#[cfg(feature = "genui")]
mod submit;
#[cfg(feature = "genui")]
mod validate;
#[cfg(feature = "genui")]
mod view;

#[cfg(feature = "genui")]
pub use a2ui::{
    A2UI_MIME_TYPE, A2UI_VERSION, A2uiBody, A2uiCapabilities, A2uiClientBody, A2uiClientMessage,
    A2uiError, A2uiMessage, CapabilitySet, ClientAction, ClientError, CreateSurface, DataBinding,
    DataModel, DeleteSurface, DynamicString, FunctionCall, UpdateComponents, UpdateDataModel,
    decode_jsonl, encode_jsonl, stable_id,
};
#[cfg(feature = "genui")]
pub use ag_ui::{A2UI_ACTION_PROP, GenuiAgUi};
pub use app_view::{
    AppAnswer, AppBlockReason, AppError, AppMessageRequest, AppOpen, AppPrompt, AppRequest, AppView,
};
#[cfg(feature = "genui")]
pub use capabilities::{GenuiWiring, capabilities, capabilities_wired};
pub use capabilities_wire::{
    CatalogView, Claim, ClaimScope, ClaimStatus, GenuiCapabilities, ProtocolClaims,
};
#[cfg(feature = "genui")]
pub use catalog::{Catalog, FORMS_CATALOG_ID};
#[cfg(feature = "genui")]
pub use draw::{DRAWN_STATE_KEY, DrawToolset, DrawnEnvelope, DrawnRecord, REFUSED_STATE_KEY};
pub use drawn::{
    DrawnActionView, DrawnArg, DrawnCheck, DrawnField, DrawnInput, DrawnKind, DrawnNode,
    DrawnOption, DrawnSent, DrawnSurface, FormState, FormStatus, RefusalKind, RefusedDrawing,
    SentVia,
};
#[cfg(feature = "genui-apps")]
pub use mcp_apps::{
    AppServers, McpAppsHost, McpAppsHostBuilder, SURFACE_AUDIT_APP_BLOCKED,
    SURFACE_AUDIT_APP_CALL_ASKED, SURFACE_AUDIT_APP_CALL_HELD, SURFACE_AUDIT_APP_CALL_REFUSED,
    SURFACE_AUDIT_APP_MESSAGE_SENT, SURFACE_AUDIT_APP_ORIGIN_REFUSED,
};
pub use status::form_states;
#[cfg(feature = "genui")]
pub use submit::DRAWN_CHANGED_CODE;
#[cfg(feature = "genui")]
pub use validate::{
    ActionArg, ComponentRef, DrawRefusal, DrawnAction, FieldArg, MAX_ACTIONS, MAX_COMPONENTS,
    MAX_DEPTH, MAX_DRAWING_BYTES, MAX_LIST_ITEMS, MAX_SHOWN, MAX_TEXT_CHARS, RESERVED_LABELS,
    RESERVED_PHRASES, ResolvedText, SurfaceDraft, ValidatedSurface, Validator, reserved,
};
#[cfg(feature = "genui")]
pub use view::SHOWN_ROWS;

#[cfg(feature = "genui")]
pub(crate) use app_view::app_name;
pub(crate) use app_view::suggested_message;
#[cfg(feature = "genui")]
pub(crate) use hints::DrawnHints;
pub(crate) use status::{is_requested, refused_on_resume, requested_via, sent};
#[cfg(feature = "genui")]
pub(crate) use submit::submission;
// The terminal's fill-in pane checks a field as the send will.
#[cfg(all(feature = "genui", feature = "tui"))]
pub(crate) use submit::{holds as check_holds, typed as typed_value};
#[cfg(feature = "genui")]
pub(crate) use view::project;
