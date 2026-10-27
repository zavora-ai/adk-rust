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
//! | The thread items ([`DrawnSurface`], [`RefusedDrawing`], [`DrawnSent`]) and [`form_states`] | What every surface shows of a drawing, and of a form sent | every build |
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
mod capabilities;
mod capabilities_wire;
#[cfg(feature = "genui")]
mod catalog;
#[cfg(feature = "genui")]
mod draw;
mod drawn;
#[cfg(feature = "genui")]
mod hints;
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
pub use capabilities::capabilities;
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
pub(crate) use hints::DrawnHints;
pub(crate) use status::{is_requested, refused_on_resume, requested_via, sent};
#[cfg(feature = "genui")]
pub(crate) use submit::submission;
// The terminal's fill-in pane checks a field as the send will.
#[cfg(all(feature = "genui", feature = "tui"))]
pub(crate) use submit::{holds as check_holds, typed as typed_value};
#[cfg(feature = "genui")]
pub(crate) use view::project;
