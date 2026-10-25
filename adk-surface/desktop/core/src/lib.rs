//! The desktop tray's logic without Tauri (spec 3.3 §5.5): which kernel the shell reaches,
//! its status as the tray icon's badge, where a link from the home or the panel goes, and where
//! the panel opens.
//!
//! The shell (`adk-desktop`) is a Tauri 2 app whose windows load the kernel's own pages: the
//! web home at `/ui/` and the tray panel at `/ui/tray`. Everything it decides that does not
//! need a window lives here, so it is tested without a display.
//!
//! | Part | What it does |
//! |---|---|
//! | [`ServerTarget`] | `--server`, else `[kernel] bind` in `harness.toml`, else `http://127.0.0.1:8080`; loopback only |
//! | [`TrayStatus`], [`Badge`] | The status line's subset the badge needs, and the badge it shows |
//! | [`Route`] | The panel, the home, an external page, or nothing |
//! | [`place_panel`] | Under or above the tray icon, inside the work area |
//! | [`StatusWatcher`] | Reads the status at start, on every kernel event and every 5 s |
//!
//! Experimental: the shell is an unpublished app, outside the library crates (spec 3.3 §6.1,
//! recorded deviation).
//!
//! # Example
//!
//! ```
//! use adk_desktop_core::{Badge, ServerTarget, TrayStatus};
//!
//! let target = ServerTarget::resolve(Some("http://0.0.0.0:8792"), None, None).unwrap();
//! assert_eq!(target.origin().as_str(), "http://127.0.0.1:8792/");
//!
//! let status: TrayStatus = serde_json::from_str(r#"{"decisions": 2, "stopped": false}"#).unwrap();
//! assert_eq!(Badge::from_status(&status), Badge::Count(2));
//! ```

mod error;
mod place;
mod route;
mod server;
mod status;
mod watch;

pub use error::{DesktopError, Result};
pub use place::{Point, Rect, Size, place_panel};
pub use route::{HOME_PATH, PANEL_PATH, Route};
pub use server::{DEFAULT_SERVER, ServerTarget, Token, user_config_file};
pub use status::{Badge, ModeView, QuietView, SpendView, TrayStatus};
pub use watch::{Reach, StatusWatcher, stop_all};
