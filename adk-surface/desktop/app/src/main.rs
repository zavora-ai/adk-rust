//! `adk-desktop`: the desktop tray of spec 3.3 §5.5. Experimental.
//!
//! Two windows load the kernel's own pages: the web home at `<server>/ui/` and the tray panel
//! at `<server>/ui/tray`. A tray icon carries the decisions badge; its menu has Show decisions,
//! Open home, Stop all and Quit. The windows get no IPC: no capability names a remote page, so a
//! page the kernel serves cannot call into the shell. Links to anything but the kernel's `/ui/`
//! pages open in the system browser or nowhere.
//!
//! ```sh
//! cargo run --manifest-path adk-surface/desktop/app/Cargo.toml --bin adk-desktop -- \
//!     --server http://127.0.0.1:8080 [--token …]
//! ```
//!
//! | Flag | Environment | Meaning |
//! |---|---|---|
//! | `--server` | `ADK_DESKTOP_SERVER` | The kernel; else `[kernel] bind` in `harness.toml`, else `http://127.0.0.1:8080` |
//! | `--token` | `ADK_SERVE_TOKEN` | The token of a kernel started with `--auth-token` |
//! | `--probe` | `ADK_DESKTOP_PROBE=1` | Opens the bundled WebKitGTK probe in place of the home |
//! | | `ADK_DESKTOP_LOG` | Also writes the log to this file |

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use adk_desktop_core::{
    Badge, CommandLine, PanelToggle, Point, Reach, Rect, Route, ServerTarget, Size, StatusWatcher,
    Toggle, TrayProblem, home_navigation, place_panel, stop_all, user_config_file,
    window_background,
};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::webview::NewWindowResponse;
use tauri::window::Color;
use tauri::{
    AppHandle, Manager, PhysicalPosition, RunEvent, Theme, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent,
};
use url::Url;

const HOME: &str = "home";
const PANEL: &str = "panel";
const TRAY: &str = "adk";
/// The panel's logical size, as the concept screens draw it.
const PANEL_SIZE: (f64, f64) = (420.0, 700.0);

const USAGE: &str = "adk-desktop: the ADK-Rust desktop tray (experimental)

Usage: adk-desktop [--server http://127.0.0.1:8080] [--token <token>] [--probe]

  --server  the kernel on this computer; default: [kernel] bind in harness.toml, else
            http://127.0.0.1:8080 (ADK_DESKTOP_SERVER)
  --token   the token of a kernel started with --auth-token (ADK_SERVE_TOKEN)
  --probe   open the bundled WebKitGTK probe in place of the home (ADK_DESKTOP_PROBE=1)";

/// The command line, over the environment.
#[derive(Debug, Default)]
struct Options {
    server: Option<String>,
    token: Option<String>,
    probe: bool,
}

fn options() -> Result<Options, String> {
    let env = |name: &str| std::env::var(name).ok().filter(|value| !value.trim().is_empty());
    let line = CommandLine::parse(std::env::args().skip(1))?;
    if line.help {
        return Err(USAGE.to_owned());
    }
    // A WebDriver host may add its own arguments; they are not ours. Only their names are
    // logged, since a value may be a token given under a mistaken flag.
    for argument in &line.ignored {
        tracing::warn!(argument = %argument, "ignoring an unknown argument");
    }
    Ok(Options {
        server: line.server.or_else(|| env("ADK_DESKTOP_SERVER")),
        token: line.token.or_else(|| env("ADK_SERVE_TOKEN")),
        probe: line.probe || env("ADK_DESKTOP_PROBE").is_some_and(|value| value == "1"),
    })
}

fn logging() {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);
    let file = std::env::var_os("ADK_DESKTOP_LOG")
        .and_then(|path| std::fs::OpenOptions::new().create(true).append(true).open(path).ok())
        .map(|file| {
            tracing_subscriber::fmt::layer().with_ansi(false).with_writer(Mutex::new(file))
        });
    tracing_subscriber::registry().with(filter).with(stderr).with(file).init();
}

fn main() {
    logging();
    let options = match options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let config = user_config_file();
    let target = match ServerTarget::resolve(
        options.server.as_deref(),
        options.token.as_deref(),
        config.as_deref(),
    ) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("adk-desktop: {error}");
            std::process::exit(2);
        }
    };
    tracing::info!(server = %target.origin(), token = target.token().is_some(), probe = options.probe, "starting the tray");

    let shell = Arc::new(Shell {
        target,
        probe: options.probe,
        away: Mutex::new(false),
        tray_built: AtomicBool::new(false),
        toggle: Mutex::new(PanelToggle::default()),
    });
    let app = tauri::Builder::default()
        .setup({
            let shell = Arc::clone(&shell);
            move |app| {
                shell.windows(app.handle())?;
                // Without a tray (no status notifier host on this desktop) the windows still work.
                match shell.tray(app.handle()) {
                    Ok(()) => shell.tray_built.store(true, Ordering::Relaxed),
                    Err(error) => tracing::warn!(error = %error, "no tray icon on this desktop"),
                }
                shell.watch(app.handle().clone());
                Ok(())
            }
        })
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        Err(error) => {
            eprintln!("adk-desktop: could not start the shell: {error}");
            std::process::exit(1);
        }
    };
    app.run(on_event);
}

fn on_event(app: &AppHandle, event: RunEvent) {
    match event {
        // Closing a window hides it; the tray keeps running until Quit.
        RunEvent::ExitRequested { api, code: None, .. } => api.prevent_exit(),
        // The dock icon clicked with every window hidden.
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => show_home(app, None),
        _ => {}
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// What the shell knows: the kernel, whether the home shows the probe, whether the windows
/// show the "Can't reach your agents" page, whether it has a tray icon, and what a click on
/// the icon does to the panel.
struct Shell {
    target: ServerTarget,
    probe: bool,
    away: Mutex<bool>,
    tray_built: AtomicBool,
    toggle: Mutex<PanelToggle>,
}

impl Shell {
    fn windows(self: &Arc<Self>, app: &AppHandle) -> tauri::Result<()> {
        // The home first: a WebDriver session drives the first webview the shell creates.
        let home_url = if self.probe {
            WebviewUrl::App("probe.html".into())
        } else {
            WebviewUrl::External(self.target.home_url())
        };
        // Shown once it has its background, so a dark desktop never sees it white.
        let home = WebviewWindowBuilder::new(app, HOME, home_url)
            .title("ADK-Rust")
            .inner_size(1280.0, 860.0)
            .visible(false)
            .initialization_script(self.target.token_script())
            .on_navigation(navigation(self, app, HOME))
            .on_new_window(new_window(self, app))
            .build()?;
        paint(&home, None);
        home.show()?;
        home.on_window_event({
            let (home, shell, app) = (home.clone(), Arc::clone(self), app.clone());
            move |event| match event {
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = home.hide();
                    shell.closed_home(&app);
                }
                WindowEvent::ThemeChanged(theme) => paint(&home, Some(*theme)),
                _ => {}
            }
        });

        let builder =
            WebviewWindowBuilder::new(app, PANEL, WebviewUrl::External(self.target.panel_url()))
                .title("ADK-Rust")
                .inner_size(PANEL_SIZE.0, PANEL_SIZE.1)
                .decorations(false)
                .always_on_top(true)
                .visible(false)
                .resizable(false)
                .initialization_script(self.target.token_script())
                .on_navigation(navigation(self, app, PANEL))
                .on_new_window(new_window(self, app));
        #[cfg(not(target_os = "macos"))]
        let builder = builder.skip_taskbar(true);
        let panel = builder.build()?;
        paint(&panel, None);
        panel.on_window_event({
            let (panel, shell) = (panel.clone(), Arc::clone(self));
            move |event| match event {
                // Hiding never decides anything (spec 3.3 §7.4 rule 1): it only hides.
                WindowEvent::Focused(false) => {
                    if panel.is_visible().unwrap_or(false) {
                        shell
                            .toggle
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .blurred(Instant::now());
                    }
                    let _ = panel.hide();
                }
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = panel.hide();
                }
                WindowEvent::ThemeChanged(theme) => paint(&panel, Some(*theme)),
                _ => {}
            }
        });
        Ok(())
    }

    /// The home window was closed, and hidden. The tray icon brings it back, and macOS's dock
    /// icon does. Without either — no tray, or a Linux desktop with no StatusNotifier host to
    /// draw it — nothing could show it again or quit, so the shell quits.
    fn closed_home(self: &Arc<Self>, app: &AppHandle) {
        #[cfg(target_os = "macos")]
        let _ = (self, app);
        #[cfg(not(target_os = "macos"))]
        {
            let (shell, app) = (Arc::clone(self), app.clone());
            tauri::async_runtime::spawn(async move {
                let drawn = shell.tray_built.load(Ordering::Relaxed)
                    && tauri::async_runtime::spawn_blocking(tray_host).await.unwrap_or(false);
                if !drawn {
                    tracing::info!("no tray icon brings the home back; quitting");
                    app.exit(0);
                }
            });
        }
    }

    fn is_away(&self) -> bool {
        *self.away.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn tray(self: &Arc<Self>, app: &AppHandle) -> tauri::Result<()> {
        let show = MenuItem::with_id(app, "show", "Show decisions", true, None::<&str>)?;
        let home = MenuItem::with_id(app, "home", "Open home", true, None::<&str>)?;
        let stop = MenuItem::with_id(app, "stop", "Stop all", true, None::<&str>)?;
        let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
        let line = PredefinedMenuItem::separator(app)?;
        let menu = Menu::with_items(app, &[&show, &home, &stop, &line, &quit])?;
        let shell = Arc::clone(self);
        let clicks = Arc::clone(self);
        TrayIconBuilder::with_id(TRAY)
            .icon(icon(Badge::Unreachable))
            .tooltip(Badge::Unreachable.tooltip())
            .menu(&menu)
            .show_menu_on_left_click(false)
            .on_menu_event(move |app, event| match event.id().as_ref() {
                // Linux emits no tray clicks, so the panel opens from here (spec 3.3 §5.5).
                "show" => show_panel(app, None),
                // Shown and focused as it is: what the person had open or typed stays.
                "home" => show_home(app, None),
                "stop" => {
                    let (target, app) = (shell.target.clone(), app.clone());
                    tauri::async_runtime::spawn(async move {
                        if let Err(error) = stop_all(&target).await {
                            tracing::warn!(error = %error, "stop all from the tray failed");
                            // The menu cannot say so; the panel does, in its banner (S-I10).
                            show_panel(&app, None);
                            if let Some(panel) = app.get_webview_window(PANEL) {
                                let _ = panel.eval(TrayProblem::of(&error).script());
                            }
                        }
                    });
                }
                // Quitting closes the shell only: nothing is decided or stopped (§7.4 rule 1).
                "quit" => app.exit(0),
                _ => {}
            })
            .on_tray_icon_event(move |tray, event| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    rect,
                    ..
                } = event
                {
                    let app = tray.app_handle();
                    let Some(panel) = app.get_webview_window(PANEL) else { return };
                    let visible = panel.is_visible().unwrap_or(false);
                    // The lock is released before the panel hides, which may report its blur.
                    let toggle = clicks
                        .toggle
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .click(Instant::now(), visible);
                    match toggle {
                        Toggle::Hide => {
                            let _ = panel.hide();
                        }
                        Toggle::Show => {
                            let position = rect.position.to_physical::<i32>(1.0);
                            let size = rect.size.to_physical::<u32>(1.0);
                            let icon = Rect {
                                origin: Point { x: position.x, y: position.y },
                                size: Size { width: size.width, height: size.height },
                            };
                            show_panel(app, Some(icon));
                        }
                        Toggle::Nothing => {}
                    }
                }
            })
            .build(app)?;
        Ok(())
    }

    /// Follows the kernel's status into the badge, and swaps both windows to the bundled
    /// "Can't reach your agents" page while the kernel is away.
    fn watch(self: &Arc<Self>, app: AppHandle) {
        let shell = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let watcher = match StatusWatcher::new(shell.target.clone()) {
                Ok(watcher) => watcher,
                Err(error) => {
                    tracing::error!(error = %error, "cannot follow the kernel");
                    return;
                }
            };
            watcher.run(|reach| shell.show(&app, reach)).await;
        });
    }

    fn show(&self, app: &AppHandle, reach: &Reach) {
        let badge = reach.badge();
        if let Some(tray) = app.tray_by_id(TRAY) {
            let _ = tray.set_icon(Some(icon(badge)));
            #[cfg(target_os = "macos")]
            let _ = tray.set_title(Some(badge.text()));
            #[cfg(not(target_os = "linux"))]
            let _ = tray.set_tooltip(Some(badge.tooltip()));
        }
        if let Some(home) = app.get_webview_window(HOME) {
            // The exact count: the icon stops at 9+, the dock and the launcher need not.
            #[cfg(not(target_os = "windows"))]
            {
                let count = reach.count().map(i64::from);
                match home.set_badge_count(count) {
                    Ok(()) => tracing::info!(badge.launcher = ?count, "launcher badge set"),
                    Err(error) => tracing::info!(error = %error, "launcher badge not set"),
                }
            }
            #[cfg(target_os = "windows")]
            let _ = home.set_overlay_icon(reach.count().map(|_| icon(badge)));
        }

        // Only a kernel that does not answer at all is away; one whose status read failed
        // keeps its pages (`Reach::Unread`).
        let away = *reach == Reach::Unreachable;
        let mut shown = self.away.lock().unwrap_or_else(PoisonError::into_inner);
        if away == *shown {
            return;
        }
        *shown = away;
        let pages: [(&str, Url); 2] = if away {
            [(HOME, bundled("index.html")), (PANEL, bundled("index.html"))]
        } else {
            [(HOME, self.target.home_url()), (PANEL, self.target.panel_url())]
        };
        for (label, url) in pages {
            if label == HOME && self.probe {
                continue;
            }
            if let Some(window) = app.get_webview_window(label) {
                tracing::info!(window = label, away, "kernel reach changed");
                let _ = window.navigate(url);
            }
        }
    }
}

/// Whether this desktop draws tray icons: a StatusNotifier host registered with the watcher.
/// Building the icon succeeds without one (stock GNOME), but it is never shown.
#[cfg(target_os = "linux")]
fn tray_host() -> bool {
    let registered = || -> zbus::Result<bool> {
        let bus = zbus::blocking::Connection::session()?;
        let watcher = zbus::blocking::Proxy::new(
            &bus,
            "org.kde.StatusNotifierWatcher",
            "/StatusNotifierWatcher",
            "org.kde.StatusNotifierWatcher",
        )?;
        watcher.get_property("IsStatusNotifierHostRegistered")
    };
    registered().unwrap_or(false)
}

/// Windows always draws the notification area's icons.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn tray_host() -> bool {
    true
}

/// Gives `window` the page's own background for `theme`, else the desktop's appearance, so
/// nothing white shows before a page paints (spec 3.3 §9).
fn paint(window: &WebviewWindow, theme: Option<Theme>) {
    let theme = theme.or_else(|| window.theme().ok());
    let [red, green, blue] = window_background(theme == Some(Theme::Dark));
    let _ = window.set_background_color(Some(Color(red, green, blue, 255)));
}

/// The badge's icon, prerendered for light and dark menu bars (`icons/tray/`).
fn icon(badge: Badge) -> Image<'static> {
    let bytes: &'static [u8] = match badge.icon() {
        "1" => include_bytes!("../icons/tray/1.png"),
        "2" => include_bytes!("../icons/tray/2.png"),
        "3" => include_bytes!("../icons/tray/3.png"),
        "4" => include_bytes!("../icons/tray/4.png"),
        "5" => include_bytes!("../icons/tray/5.png"),
        "6" => include_bytes!("../icons/tray/6.png"),
        "7" => include_bytes!("../icons/tray/7.png"),
        "8" => include_bytes!("../icons/tray/8.png"),
        "9" => include_bytes!("../icons/tray/9.png"),
        "9plus" => include_bytes!("../icons/tray/9plus.png"),
        "stopped" => include_bytes!("../icons/tray/stopped.png"),
        "unreachable" => include_bytes!("../icons/tray/unreachable.png"),
        "signin" => include_bytes!("../icons/tray/signin.png"),
        _ => include_bytes!("../icons/tray/idle.png"),
    };
    Image::from_bytes(bytes).unwrap_or_else(|_| Image::new_owned(vec![0; 4], 1, 1))
}

/// A page bundled in the shell: `tauri://localhost/<page>`, or `http://tauri.localhost/<page>`
/// on Windows.
fn bundled(page: &str) -> Url {
    let base = if cfg!(windows) { "http://tauri.localhost/" } else { "tauri://localhost/" };
    Url::parse(&format!("{base}{page}")).expect("a bundled page has a valid URL")
}

/// Which pages a window may show: the home window the kernel's `/ui/` pages, the panel its
/// `/ui/tray`, both the bundled ones. A home page linked from the panel opens in the home
/// window; any other web page in the system browser; anything else nowhere.
fn navigation(
    shell: &Arc<Shell>,
    app: &AppHandle,
    window: &'static str,
) -> impl Fn(&Url) -> bool + Send + 'static {
    let (shell, app) = (Arc::clone(shell), app.clone());
    move |url| match Route::classify(url, &shell.target) {
        Route::Bundled => true,
        Route::Home | Route::Panel if window == HOME => true,
        Route::Panel => true,
        Route::Home => {
            later(&shell, &app, url.clone(), Route::Home);
            false
        }
        Route::External => {
            later(&shell, &app, url.clone(), Route::External);
            false
        }
        Route::Drop => {
            tracing::warn!(scheme = url.scheme(), "dropped a link");
            false
        }
    }
}

/// `window.open` and `target="_blank"`: never a new window. A home page opens in the home
/// window, another web page in the system browser.
fn new_window(
    shell: &Arc<Shell>,
    app: &AppHandle,
) -> impl Fn(Url, tauri::webview::NewWindowFeatures) -> NewWindowResponse<tauri::Wry> + Send + 'static
{
    let (shell, app) = (Arc::clone(shell), app.clone());
    move |url, _features| {
        match Route::classify(&url, &shell.target) {
            Route::Home | Route::Panel => later(&shell, &app, url, Route::Home),
            Route::External => later(&shell, &app, url, Route::External),
            Route::Bundled | Route::Drop => {
                tracing::warn!(scheme = url.scheme(), "dropped a new window")
            }
        }
        NewWindowResponse::Deny
    }
}

/// Acts on a link after the webview's callback returns, off the event loop. A link to the
/// home itself only shows the home window; a page inside it is loaded there.
fn later(shell: &Arc<Shell>, app: &AppHandle, url: Url, route: Route) {
    let (shell, app) = (Arc::clone(shell), app.clone());
    tauri::async_runtime::spawn(async move {
        match route {
            Route::Home => show_home(&app, home_navigation(&url, shell.is_away())),
            _ => {
                if let Err(error) = tauri_plugin_opener::open_url(url.as_str(), None::<&str>) {
                    tracing::warn!(error = %error, "could not open the browser");
                }
            }
        }
    });
}

fn show_home(app: &AppHandle, url: Option<Url>) {
    if let Some(panel) = app.get_webview_window(PANEL) {
        let _ = panel.hide();
    }
    if let Some(home) = app.get_webview_window(HOME) {
        if let Some(url) = url {
            let _ = home.navigate(url);
        }
        let _ = home.show();
        let _ = home.unminimize();
        let _ = home.set_focus();
    }
}

fn show_panel(app: &AppHandle, tray: Option<Rect>) {
    let Some(panel) = app.get_webview_window(PANEL) else { return };
    if let Some(at) = panel_position(&panel, tray) {
        let _ = panel.set_position(PhysicalPosition::new(at.x, at.y));
    }
    let _ = panel.show();
    let _ = panel.set_focus();
}

fn panel_position(panel: &WebviewWindow, tray: Option<Rect>) -> Option<Point> {
    let monitor = match tray {
        Some(icon) => panel
            .monitor_from_point(f64::from(icon.origin.x), f64::from(icon.origin.y))
            .ok()
            .flatten(),
        None => None,
    }
    .or_else(|| panel.primary_monitor().ok().flatten())?;
    let area = monitor.work_area();
    let work_area = Rect {
        origin: Point { x: area.position.x, y: area.position.y },
        size: Size { width: area.size.width, height: area.size.height },
    };
    let scale = monitor.scale_factor();
    // The panel's size in physical pixels on this monitor.
    let size = Size {
        width: (PANEL_SIZE.0 * scale).round() as u32,
        height: (PANEL_SIZE.1 * scale).round() as u32,
    };
    Some(place_panel(tray, work_area, size))
}
