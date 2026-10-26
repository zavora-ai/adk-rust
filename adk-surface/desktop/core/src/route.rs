//! Where a link from the home or the panel goes (spec 3.3 §5.5): the shell's windows show
//! only the kernel's own pages and the shell's bundled ones; everything else opens in the
//! browser or nowhere.

use url::Url;

use crate::ServerTarget;

/// The web home's path on the kernel.
pub const HOME_PATH: &str = "/ui/";

/// The tray panel's path on the kernel.
pub const PANEL_PATH: &str = "/ui/tray";

/// Where a URL a window navigates to or opens belongs.
///
/// # Example
///
/// ```
/// use adk_desktop_core::{Route, ServerTarget};
/// use url::Url;
///
/// let server = ServerTarget::resolve(Some("http://127.0.0.1:8792"), None, None).unwrap();
/// let route = |url: &str| Route::classify(&Url::parse(url).unwrap(), &server);
/// assert_eq!(route("http://127.0.0.1:8792/ui/tray"), Route::Panel);
/// assert_eq!(route("http://127.0.0.1:8792/ui/decisions/apr_1"), Route::Home);
/// assert_eq!(route("https://adk-rust.com/docs"), Route::External);
/// assert_eq!(route("http://127.0.0.1:9999/ui/"), Route::External);
/// assert_eq!(route("file:///etc/passwd"), Route::Drop);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Route {
    /// The tray panel: the server's `/ui/tray`.
    Panel,
    /// The web home: the server's other `/ui/` pages.
    Home,
    /// The shell's own bundled pages (the "Can't reach your agents" page, the probe):
    /// `tauri://localhost`, or `http(s)://tauri.localhost` on Windows.
    Bundled,
    /// Any other `http` or `https` page, handed to the system browser.
    External,
    /// Anything else, which goes nowhere.
    Drop,
}

impl Route {
    /// Classifies `url` against `server`.
    pub fn classify(url: &Url, server: &ServerTarget) -> Self {
        if url.scheme() == "tauri" && url.host_str() == Some("localhost") {
            return Self::Bundled;
        }
        if !matches!(url.scheme(), "http" | "https") {
            return Self::Drop;
        }
        if url.host_str() == Some("tauri.localhost") {
            return Self::Bundled;
        }
        if !server.same_origin(url) {
            return Self::External;
        }
        let path = url.path();
        if path == PANEL_PATH || path.starts_with("/ui/tray/") {
            Self::Panel
        } else if path == "/ui" || path.starts_with(HOME_PATH) {
            Self::Home
        } else {
            Self::External
        }
    }
}

/// What a link to the home does to the home window (spec 3.3 §5.5: "the home window, shown
/// and focused"): `None` shows the window as it is, `Some` loads that page first.
///
/// The home itself (`/ui/`) never reloads the window, which would lose what the person had
/// open or typed there; a page inside it, such as a decision at `/ui/decisions/<id>`, is loaded.
/// While the kernel is away (`away`) nothing is loaded: the window keeps the bundled "Can't
/// reach your agents" page until the kernel answers.
///
/// # Example
///
/// ```
/// use adk_desktop_core::home_navigation;
/// use url::Url;
///
/// let home = Url::parse("http://127.0.0.1:8792/ui/").unwrap();
/// let decision = Url::parse("http://127.0.0.1:8792/ui/decisions/apr_1").unwrap();
/// assert_eq!(home_navigation(&home, false), None);
/// assert_eq!(home_navigation(&decision, false), Some(decision.clone()));
/// assert_eq!(home_navigation(&decision, true), None);
/// ```
pub fn home_navigation(link: &Url, away: bool) -> Option<Url> {
    let root = matches!(link.path(), "/ui" | HOME_PATH)
        && link.query().is_none()
        && link.fragment().is_none();
    (!away && !root).then(|| link.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_page_inside_the_home_reloads_the_home_window() {
        let at = |path: &str| Url::parse(&format!("http://127.0.0.1:8792{path}")).unwrap();
        for (path, away, loads) in [
            ("/ui/", false, false),
            ("/ui", false, false),
            ("/ui/decisions/apr_1", false, true),
            ("/ui/today", false, true),
            ("/ui/?section=memory", false, true),
            ("/ui/#top", false, true),
            ("/ui/", true, false),
            ("/ui/decisions/apr_1", true, false),
        ] {
            let link = at(path);
            assert_eq!(home_navigation(&link, away), loads.then(|| link.clone()), "{path} {away}");
        }
    }

    #[test]
    fn the_kernel_s_pages_its_api_and_other_schemes() {
        let server = ServerTarget::resolve(Some("http://127.0.0.1:8792"), None, None).unwrap();
        let route = |url: &str| Route::classify(&Url::parse(url).unwrap(), &server);
        for (url, expected) in [
            ("http://127.0.0.1:8792/ui/tray", Route::Panel),
            ("http://127.0.0.1:8792/ui/tray/", Route::Panel),
            ("http://127.0.0.1:8792/ui/", Route::Home),
            ("http://127.0.0.1:8792/ui", Route::Home),
            ("http://127.0.0.1:8792/ui/today?x=1#y", Route::Home),
            ("http://127.0.0.1:8792/ui/trayx", Route::Home),
            ("http://127.0.0.1:8792/api/kernel/away/lines/4", Route::External),
            ("http://127.0.0.1:8792/", Route::External),
            ("http://localhost:8792/ui/", Route::External),
            ("https://127.0.0.1:8792/ui/", Route::External),
            ("tauri://localhost/index.html", Route::Bundled),
            ("http://tauri.localhost/index.html", Route::Bundled),
            ("https://tauri.localhost/probe.html", Route::Bundled),
            ("tauri://elsewhere/index.html", Route::Drop),
            ("javascript:alert(1)", Route::Drop),
            ("data:text/html,hi", Route::Drop),
            ("mailto:ana@example.com", Route::Drop),
            ("about:blank", Route::Drop),
        ] {
            assert_eq!(route(url), expected, "{url}");
        }
    }
}
