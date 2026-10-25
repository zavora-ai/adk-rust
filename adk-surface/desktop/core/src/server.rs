//! Which kernel the shell reaches (spec 3.3 §5.5): one on this computer, over plain HTTP.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use url::{Host, Url};

use crate::{DesktopError, Result};

/// Where `adk-rust up` listens unless told otherwise.
pub const DEFAULT_SERVER: &str = "http://127.0.0.1:8080";

/// The kernel's token (`adk-rust up --auth-token`). Never logged: its `Debug` hides it, and it
/// has no `Display`.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// The token, or `None` when `value` is empty or blank.
    pub fn new(value: &str) -> Option<Self> {
        let value = value.trim();
        (!value.is_empty()).then(|| Self(value.to_owned()))
    }

    /// The token itself, for the `Authorization` header and the page's sign-in.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(…)")
    }
}

/// The kernel the shell reaches, and its token when it was started with one.
///
/// # Example
///
/// ```
/// use adk_desktop_core::ServerTarget;
///
/// let target = ServerTarget::resolve(Some("127.0.0.1:8792"), Some("t0ken"), None).unwrap();
/// assert_eq!(target.home_url().as_str(), "http://127.0.0.1:8792/ui/");
/// assert_eq!(target.panel_url().as_str(), "http://127.0.0.1:8792/ui/tray");
/// assert_eq!(format!("{:?}", target.token()), "Some(Token(…))");
/// assert!(ServerTarget::resolve(Some("http://192.168.1.4:8080"), None, None).is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerTarget {
    origin: Url,
    token: Option<Token>,
}

/// The `[kernel]` table of the user's `harness.toml`, as far as the tray reads it.
#[derive(Debug, Default, Deserialize)]
struct UserConfig {
    #[serde(default)]
    kernel: Option<KernelTable>,
}

#[derive(Debug, Default, Deserialize)]
struct KernelTable {
    #[serde(default)]
    bind: Option<String>,
}

impl ServerTarget {
    /// Resolves the kernel (spec 3.3 §5.5): `server` (`--server`), else `[kernel] bind` in
    /// `config` (normally [`user_config_file`]), else [`DEFAULT_SERVER`]. An unspecified bind
    /// (`0.0.0.0`, `::`) is reached on loopback; any other host that is not this computer is
    /// refused. `token` is `--token` or `ADK_SERVE_TOKEN`.
    ///
    /// # Errors
    ///
    /// [`DesktopError::BadServer`] for an address that is not plain HTTP to a host and port,
    /// [`DesktopError::NotLoopback`] for another machine, and [`DesktopError::Config`] when
    /// `config` exists but cannot be read.
    pub fn resolve(
        server: Option<&str>,
        token: Option<&str>,
        config: Option<&Path>,
    ) -> Result<Self> {
        let origin = match server.map(str::trim).filter(|value| !value.is_empty()) {
            Some(value) => parse_server(value)?,
            None => match config.map(read_bind).transpose()?.flatten() {
                Some(bind) => origin_of(bind)?,
                None => parse_server(DEFAULT_SERVER)?,
            },
        };
        Ok(Self { origin, token: token.and_then(Token::new) })
    }

    /// The server's origin, `http://127.0.0.1:8080/`.
    pub fn origin(&self) -> &Url {
        &self.origin
    }

    /// The token, when the kernel needs one.
    pub fn token(&self) -> Option<&Token> {
        self.token.as_ref()
    }

    /// The web home, `<origin>/ui/`.
    pub fn home_url(&self) -> Url {
        self.join(crate::HOME_PATH)
    }

    /// The tray panel, `<origin>/ui/tray`.
    pub fn panel_url(&self) -> Url {
        self.join(crate::PANEL_PATH)
    }

    /// A route of the server's API, `<origin>/api<path>`; `path` may carry a query.
    pub fn api(&self, path: &str) -> Url {
        self.join(&format!("/api{path}"))
    }

    /// The origin as a page sees it (`location.origin`): `http://127.0.0.1:8080`.
    pub fn page_origin(&self) -> String {
        self.origin.origin().ascii_serialization()
    }

    /// Whether `url` is on this server.
    pub fn same_origin(&self, url: &Url) -> bool {
        url.origin() == self.origin.origin()
    }

    /// `reference` (a path, with a query when it has one) on the server.
    fn join(&self, reference: &str) -> Url {
        // The origin's path is `/`, so a relative reference resolves under it.
        self.origin.join(reference.trim_start_matches('/')).unwrap_or_else(|_| self.origin.clone())
    }
}

/// The user's `harness.toml`: `<config dir>/adk-rust/harness.toml`, where `adk-rust up` reads
/// its `[kernel]` table.
pub fn user_config_file() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("adk-rust").join("harness.toml"))
}

fn read_bind(path: &Path) -> Result<Option<SocketAddr>> {
    let invalid = |reason: String| DesktopError::Config { path: path.to_path_buf(), reason };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(invalid(error.to_string())),
    };
    let config: UserConfig = toml::from_str(&text).map_err(|error| invalid(error.to_string()))?;
    config
        .kernel
        .and_then(|kernel| kernel.bind)
        .map(|bind| {
            bind.parse::<SocketAddr>().map_err(|error| invalid(format!("bind `{bind}`: {error}")))
        })
        .transpose()
}

fn origin_of(bind: SocketAddr) -> Result<Url> {
    let host = match loopback(bind.ip()) {
        Some(IpAddr::V4(ip)) => ip.to_string(),
        Some(IpAddr::V6(ip)) => format!("[{ip}]"),
        None => return Err(DesktopError::NotLoopback { host: bind.ip().to_string() }),
    };
    parse_server(&format!("http://{host}:{}", bind.port()))
}

/// `ip` as the tray reaches it: loopback as it is, unspecified as loopback, else `None`.
fn loopback(ip: IpAddr) -> Option<IpAddr> {
    match ip {
        IpAddr::V4(ip) if ip.is_unspecified() => Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        IpAddr::V6(ip) if ip.is_unspecified() => Some(IpAddr::V6(Ipv6Addr::LOCALHOST)),
        ip if ip.is_loopback() => Some(ip),
        _ => None,
    }
}

fn parse_server(value: &str) -> Result<Url> {
    let bad = |reason: &str| DesktopError::BadServer {
        value: value.to_owned(),
        reason: reason.to_owned(),
    };
    let text = if value.contains("://") { value.to_owned() } else { format!("http://{value}") };
    let mut url = Url::parse(&text).map_err(|error| bad(&error.to_string()))?;
    match url.scheme() {
        "http" => {}
        "https" => {
            return Err(bad(
                "the tray speaks plain HTTP to a kernel on this computer; use http://",
            ));
        }
        _ => return Err(bad("use an http:// address")),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(bad("an address with a user name or password"));
    }
    if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
        return Err(bad("give the server only, without a path"));
    }
    let not_loopback = |host: String| DesktopError::NotLoopback { host };
    let reached = match url.host().map(|host| host.to_owned()) {
        Some(Host::Ipv4(ip)) => {
            Some(loopback(IpAddr::V4(ip)).ok_or_else(|| not_loopback(ip.to_string()))?)
        }
        Some(Host::Ipv6(ip)) => {
            Some(loopback(IpAddr::V6(ip)).ok_or_else(|| not_loopback(ip.to_string()))?)
        }
        Some(Host::Domain(name)) if name.eq_ignore_ascii_case("localhost") => None,
        Some(Host::Domain(name)) => return Err(not_loopback(name)),
        None => return Err(bad("no host")),
    };
    if let Some(ip) = reached {
        url.set_ip_host(ip).map_err(|()| bad("cannot use this host"))?;
    }
    url.set_path("/");
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(server: &str) -> String {
        ServerTarget::resolve(Some(server), None, None).unwrap().origin().to_string()
    }

    #[test]
    fn a_given_server_wins_and_is_normalised() {
        assert_eq!(origin("http://127.0.0.1:8792"), "http://127.0.0.1:8792/");
        assert_eq!(origin("127.0.0.1:8792/"), "http://127.0.0.1:8792/");
        assert_eq!(origin("http://localhost:9000"), "http://localhost:9000/");
        assert_eq!(origin("http://[::1]:9000"), "http://[::1]:9000/");
        assert_eq!(origin("http://127.0.0.1"), "http://127.0.0.1/");
    }

    #[test]
    fn an_unspecified_address_is_reached_on_loopback() {
        assert_eq!(origin("http://0.0.0.0:8080"), "http://127.0.0.1:8080/");
        assert_eq!(origin("http://[::]:8080"), "http://[::1]:8080/");
    }

    #[test]
    fn another_machine_tls_and_paths_are_refused_with_guidance() {
        for (server, needle) in [
            ("http://192.168.1.4:8080", "on this computer only"),
            ("http://kernel.example.com", "on this computer only"),
            ("https://127.0.0.1:8080", "plain HTTP"),
            ("ftp://127.0.0.1", "http://"),
            ("http://127.0.0.1:8080/ui/", "without a path"),
            ("http://ana:pw@127.0.0.1:8080", "user name"),
        ] {
            let error = ServerTarget::resolve(Some(server), None, None).unwrap_err().to_string();
            assert!(error.contains(needle), "{server}: {error}");
        }
    }

    #[test]
    fn the_order_is_flag_then_harness_toml_then_the_default() {
        let dir = std::env::temp_dir().join(format!("adk-desktop-core-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("harness.toml");
        std::fs::write(
            &config,
            "mode = \"ask\"\n[kernel]\nbind = \"0.0.0.0:9090\"\ncolour = \"blue\"\n",
        )
        .unwrap();

        let flag = ServerTarget::resolve(Some("127.0.0.1:7000"), None, Some(&config)).unwrap();
        assert_eq!(flag.origin().as_str(), "http://127.0.0.1:7000/");
        let file = ServerTarget::resolve(None, None, Some(&config)).unwrap();
        assert_eq!(file.origin().as_str(), "http://127.0.0.1:9090/");
        let blank = ServerTarget::resolve(Some("  "), None, Some(&config)).unwrap();
        assert_eq!(blank, file);

        std::fs::write(&config, "[kernel]\nmax_concurrent_runs = 2\n").unwrap();
        let default = ServerTarget::resolve(None, None, Some(&config)).unwrap();
        assert_eq!(default.origin().as_str(), "http://127.0.0.1:8080/");
        let missing = ServerTarget::resolve(None, None, Some(&dir.join("none.toml"))).unwrap();
        assert_eq!(missing, default);

        std::fs::write(&config, "[kernel]\nbind = \"10.0.0.2:8080\"\n").unwrap();
        let remote = ServerTarget::resolve(None, None, Some(&config)).unwrap_err();
        assert!(matches!(remote, DesktopError::NotLoopback { .. }), "{remote}");
        std::fs::write(&config, "[kernel]\nbind = \"not an address\"\n").unwrap();
        let invalid = ServerTarget::resolve(None, None, Some(&config)).unwrap_err();
        assert!(matches!(invalid, DesktopError::Config { .. }), "{invalid}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_token_is_kept_unless_blank_and_never_shown() {
        let target = ServerTarget::resolve(None, Some(" s3cret "), None).unwrap();
        assert_eq!(target.token().map(Token::expose), Some("s3cret"));
        assert!(!format!("{target:?}").contains("s3cret"));
        assert_eq!(ServerTarget::resolve(None, Some("  "), None).unwrap().token(), None);
    }

    #[test]
    fn urls_on_the_server() {
        let target = ServerTarget::resolve(Some("http://127.0.0.1:8792"), None, None).unwrap();
        assert_eq!(
            target.api("/surface/status?surface=tray").as_str(),
            "http://127.0.0.1:8792/api/surface/status?surface=tray"
        );
        assert_eq!(target.page_origin(), "http://127.0.0.1:8792");
        assert!(target.same_origin(&Url::parse("http://127.0.0.1:8792/ui/today").unwrap()));
        assert!(!target.same_origin(&Url::parse("http://127.0.0.1:8793/ui/").unwrap()));
    }
}
