use adk_studio::{
    AppState, FileStorage, api_routes, cleanup_stale_sessions, embedded, start_scheduler,
};
use axum::{Router, extract::Path as AxumPath, routing::get};
use std::net::SocketAddr;
use std::path::PathBuf;
use tower_http::{
    cors::{AllowOrigin, Any, CorsLayer},
    services::{ServeDir, ServeFile},
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Parsed CLI configuration for the adk-studio binary.
#[derive(Debug, Clone)]
pub struct CliConfig {
    pub port: u16,
    pub host: [u8; 4],
    pub projects_dir: PathBuf,
    pub static_dir: Option<PathBuf>,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            port: 3000,
            host: [127, 0, 0, 1],
            projects_dir: dirs::data_local_dir().unwrap_or_default().join("adk-studio/projects"),
            static_dir: None,
        }
    }
}

/// Result of parsing CLI arguments.
#[derive(Debug)]
pub enum CliAction {
    /// Print version and exit.
    PrintVersion,
    /// Print help and exit.
    PrintHelp,
    /// Start the server with this config.
    Run(CliConfig),
}

/// Parse CLI arguments into a `CliAction`.
///
/// `args` should be the arguments *after* the binary name (i.e., `std::env::args().skip(1)`).
///
/// Scanning order:
/// 1. `--version` / `-V` (highest priority)
/// 2. `--help`
/// 3. Iterate through args, matching known flags and consuming values
/// 4. Reject unknown flags (anything starting with `-`)
pub fn parse_args(args: &[String]) -> Result<CliAction, String> {
    // 1. Scan for --version / -V first (highest priority)
    if args.iter().any(|a| a == "--version" || a == "-V") {
        return Ok(CliAction::PrintVersion);
    }

    // 2. Scan for --help (second priority)
    if args.iter().any(|a| a == "--help") {
        return Ok(CliAction::PrintHelp);
    }

    // 3. Iterate through args, matching known flags
    let mut config = CliConfig::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--port" | "-p" => {
                let val =
                    args.get(i + 1).ok_or_else(|| format!("flag '{arg}' requires a value"))?;
                config.port =
                    val.parse::<u16>().map_err(|_| format!("invalid port value '{val}'"))?;
                i += 2;
            }
            "--host" | "-h" => {
                let val =
                    args.get(i + 1).ok_or_else(|| format!("flag '{arg}' requires a value"))?;
                config.host = parse_host(val)?;
                i += 2;
            }
            "--dir" | "-d" => {
                let val =
                    args.get(i + 1).ok_or_else(|| format!("flag '{arg}' requires a value"))?;
                config.projects_dir = PathBuf::from(val);
                i += 2;
            }
            "--static" | "-s" => {
                let val =
                    args.get(i + 1).ok_or_else(|| format!("flag '{arg}' requires a value"))?;
                config.static_dir = Some(PathBuf::from(val));
                i += 2;
            }
            other if other.starts_with('-') => {
                return Err(format!(
                    "unknown flag '{other}'\nRun 'adk-studio --help' for usage information."
                ));
            }
            _ => {
                // Positional argument — skip (not an error for forward compat)
                i += 1;
            }
        }
    }

    Ok(CliAction::Run(config))
}

/// Parse a host string like "a.b.c.d" into `[u8; 4]`.
fn parse_host(s: &str) -> Result<[u8; 4], String> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return Err(format!("invalid host '{s}': expected format a.b.c.d"));
    }
    let mut octets = [0u8; 4];
    for (i, part) in parts.iter().enumerate() {
        octets[i] = part
            .parse::<u8>()
            .map_err(|_| format!("invalid host '{s}': '{part}' is not a valid octet"))?;
    }
    Ok(octets)
}

/// Print the version string to stdout.
pub fn print_version() {
    println!("adk-studio {}", env!("CARGO_PKG_VERSION"));
}

/// Print usage information to stdout.
pub fn print_help() {
    println!("adk-studio {}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("Visual development environment for ADK-Rust agents");
    println!();
    println!("USAGE:");
    println!("    adk-studio [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("    -p, --port <PORT>      Server port [default: 3000]");
    println!("    -h, --host <HOST>      Server host [default: 127.0.0.1]");
    println!("    -d, --dir <DIR>        Projects directory");
    println!("    -s, --static <DIR>     Static files directory");
    println!("    -V, --version          Print version and exit");
    println!("        --help             Print this help message and exit");
}

/// Handler for serving embedded static files
async fn serve_static(AxumPath(path): AxumPath<String>) -> axum::response::Response {
    embedded::serve_embedded(path)
}

/// Handler for serving index.html at root
async fn serve_root() -> axum::response::Response {
    embedded::serve_index()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let config = match parse_args(&args) {
        Ok(CliAction::PrintVersion) => {
            print_version();
            return Ok(());
        }
        Ok(CliAction::PrintHelp) => {
            print_help();
            return Ok(());
        }
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!("Run 'adk-studio --help' for usage information.");
            std::process::exit(1);
        }
        Ok(CliAction::Run(config)) => config,
    };

    // Load .env file if present
    dotenvy::dotenv().ok();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let port = config.port;
    let host = config.host;
    let projects_dir = config.projects_dir;
    let static_dir = config.static_dir;

    let storage = FileStorage::new(projects_dir.clone()).await?;
    let state = AppState::new(storage);

    // Start the schedule trigger service in the background
    let scheduler_state = state.clone();
    tokio::spawn(async move {
        start_scheduler(scheduler_state).await;
    });

    // Start periodic session cleanup (every 10 minutes, remove sessions older than 1 hour)
    tokio::spawn(async {
        let cleanup_interval = std::time::Duration::from_secs(600);
        let max_session_age = std::time::Duration::from_secs(3600);
        loop {
            tokio::time::sleep(cleanup_interval).await;
            cleanup_stale_sessions(max_session_age).await;
        }
    });

    // Restrict CORS to localhost origins to prevent drive-by attacks
    // while allowing local development and usage.
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _| {
            let origin_str = origin.to_str().unwrap_or("");
            if let Some(rest) =
                origin_str.strip_prefix("http://").or_else(|| origin_str.strip_prefix("https://"))
            {
                const ALLOWED_HOSTS: &[&str] = &["localhost", "127.0.0.1", "[::1]"];
                ALLOWED_HOSTS
                    .iter()
                    .any(|host| rest == *host || rest.starts_with(&format!("{}:", host)))
            } else {
                false
            }
        }))
        .allow_methods(Any)
        .allow_headers(Any);

    let mut app = Router::new().nest("/api", api_routes()).layer(cors).with_state(state);

    // Serve static files - external directory takes priority, otherwise use embedded
    if let Some(dir) = static_dir {
        let index = dir.join("index.html");
        app = app.fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(index)));
        tracing::info!("📂 Serving static files from: {}", dir.display());
    } else {
        // Serve embedded static files (default)
        // Use a nested router for static files to avoid route conflicts
        let static_router =
            Router::new().route("/", get(serve_root)).route("/*path", get(serve_static));
        app = app.merge(static_router);
        tracing::info!("📦 Serving embedded static files");
    }

    let addr = SocketAddr::from((host, port));
    tracing::info!("🚀 ADK Studio starting on http://{}", addr);
    tracing::info!("📁 Projects directory: {}", projects_dir.display());

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
