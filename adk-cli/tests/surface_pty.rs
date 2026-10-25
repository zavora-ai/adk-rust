//! The terminal surface in the `adk-rust` binary (spec 3.3 §5.2), in a pseudo-terminal read
//! by a vt100 parser: full screen, inline and JSON on the harness with a scripted model.
//!
//! Each test drives the real binary: a held call becomes a decision box from the approvals
//! inbox, keys answer it, typing while it is open is queued, Esc never decides, model text
//! cannot reach the terminal as control characters, and the terminal is restored on a signal.

#![cfg(all(unix, feature = "scripted-model", feature = "tui"))]

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{TIMEOUT, Terminal, run, text};
use serde_json::Value;

const ROWS: u16 = 40;
const COLS: u16 = 150;

/// A held write, then the reply after it ran, then the reply to a queued message.
const WRITE_THEN_REPLY: &str = r#"[
    {"call": {"name": "write_file", "args": {"path": "invoice.txt", "content": "INV-1"}}},
    {"text": "The invoice is written."},
    {"text": "Copied your accountant."}
]"#;

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "a project\n").unwrap();
    dir
}

/// The binary as the terminal surface, in process, on `ws`.
fn surface(home: &Path, ws: &Path, script: &str, extra: &[&str]) -> Terminal {
    let dir = ws.display().to_string();
    let mut args =
        vec!["--ephemeral", "--dir", dir.as_str(), "--model-script", script, "--mode", "ask"];
    args.extend_from_slice(extra);
    Terminal::spawn_sized(&args, home, ws, ROWS, COLS)
}

/// What a terminal of `ROWS` by `COLS` shows after the binary's output so far.
fn screen(terminal: &Terminal) -> vt100::Parser {
    let mut parser = vt100::Parser::new(ROWS, COLS, 0);
    parser.process(&terminal.bytes());
    parser
}

/// Waits until the screen shows `needle`; returns the screen.
fn expect_screen(terminal: &Terminal, needle: &str) -> vt100::Parser {
    expect_all(terminal, &[needle])
}

/// Waits until the screen shows every one of `needles` at once, so a frame read while it was
/// still being drawn never fails a test; returns the screen.
fn expect_all(terminal: &Terminal, needles: &[&str]) -> vt100::Parser {
    let started = Instant::now();
    loop {
        let parser = screen(terminal);
        let shown = parser.screen().contents();
        if needles.iter().all(|needle| shown.contains(needle)) {
            return parser;
        }
        assert!(
            started.elapsed() < TIMEOUT,
            "timed out waiting for {needles:?} on screen:\n{shown}"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Waits for the binary to exit; returns its code.
fn wait_exit(terminal: Terminal) -> (Option<i32>, String) {
    terminal.wait()
}

/// Writes the screen as text and as HTML into `$ADK_TUI_SCREENS`, when set, for the PR's
/// screenshots; a name ending in `-dark` is drawn on a dark terminal.
fn dump(name: &str, parser: &vt100::Parser) {
    let Ok(dir) = std::env::var("ADK_TUI_SCREENS") else {
        return;
    };
    let dir = Path::new(&dir);
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(format!("{name}.txt")), parser.screen().contents()).unwrap();
    let html = html(parser.screen(), name.ends_with("-dark"));
    std::fs::write(dir.join(format!("{name}.html")), html).unwrap();
}

fn css(color: vt100::Color, default: &str) -> String {
    match color {
        vt100::Color::Default => default.to_string(),
        vt100::Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        vt100::Color::Idx(index) => {
            let (r, g, b) = xterm(index);
            format!("#{r:02x}{g:02x}{b:02x}")
        }
    }
}

fn xterm(index: u8) -> (u8, u8, u8) {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
        (127, 127, 127),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (92, 92, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    match index {
        0..=15 => BASE[usize::from(index)],
        16..=231 => {
            let i = index - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            (level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        _ => {
            let grey = 8 + (index - 232) * 10;
            (grey, grey, grey)
        }
    }
}

/// The screen as HTML: every cell with its colours, in a monospace grid, on a light or dark
/// terminal's default colours.
fn html(screen: &vt100::Screen, dark: bool) -> String {
    let (rows, cols) = screen.size();
    let span = |style: &str, text: &str, wide: u16| {
        format!("<span style=\"{style}width:{wide}ch\">{text}</span>")
    };
    let mut body = String::new();
    for row in 0..rows {
        body.push_str("<div class=\"row\">");
        // Cells of one style share a span, so lines of box characters are drawn unbroken.
        let mut run: Option<(String, String, u16)> = None;
        let mut col = 0;
        while col < cols {
            let cell = screen.cell(row, col).unwrap();
            if cell.is_wide_continuation() {
                col += 1;
                continue;
            }
            let (mut fg, mut bg) =
                (css(cell.fgcolor(), "var(--fg)"), css(cell.bgcolor(), "var(--bg)"));
            if cell.inverse() {
                std::mem::swap(&mut fg, &mut bg);
            }
            let mut style = format!("color:{fg};background:{bg};");
            if cell.bold() {
                style.push_str("font-weight:700;");
            }
            if cell.italic() {
                style.push_str("font-style:italic;");
            }
            if cell.underline() {
                style.push_str("text-decoration:underline;");
            }
            if cell.dim() {
                style.push_str("opacity:0.6;");
            }
            let contents = if cell.has_contents() { cell.contents() } else { " " };
            let escaped = contents.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            let width = if cell.is_wide() { 2 } else { 1 };
            match &mut run {
                Some((shared, text, wide)) if *shared == style => {
                    text.push_str(&escaped);
                    *wide += width;
                }
                _ => {
                    if let Some((style, text, wide)) = run.replace((style, escaped, width)) {
                        body.push_str(&span(&style, &text, wide));
                    }
                }
            }
            col += 1;
        }
        if let Some((style, text, wide)) = run {
            body.push_str(&span(&style, &text, wide));
        }
        body.push_str("</div>\n");
    }
    let (fg, bg) = if dark { ("#f1f5f9", "#0a1628") } else { ("#0f172a", "#ffffff") };
    format!(
        "<!doctype html><meta charset=\"utf-8\"><style>:root{{--fg:{fg};--bg:{bg}}}\
         body{{margin:0;background:var(--bg)}}\
         .screen{{display:inline-block;font:14px/16px 'JetBrains Mono',Menlo,monospace;}}\
         .row{{display:flex;height:16px}}.row span{{display:inline-block;white-space:pre;\
         overflow:hidden}}</style><div class=\"screen\">{body}</div>"
    )
}

#[test]
fn full_screen_answers_a_held_call_by_key() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let mut terminal = surface(home.path(), ws.path(), WRITE_THEN_REPLY, &["--theme", "light"]);
    let first = ["Type a message", "adk-rust", "Agents", "Coming up", "While you were away"];
    let parser = expect_all(&terminal, &[&first[..], &["esc never decides"]].concat());
    assert!(parser.screen().alternate_screen(), "full screen takes the alternate screen");

    terminal.send("Write the October invoice\r");
    // The badge counts the decision.
    let parser = expect_all(
        &terminal,
        &[
            "Waiting for you · Write file invoice.txt",
            "Medium risk · Can't be undone",
            "Allow once",
            "Deny",
            "Decisions  1",
        ],
    );
    assert!(!ws.path().join("invoice.txt").exists(), "held, not written");
    dump("full-screen-decision-light", &parser);

    // The box arms after it has been on screen a moment; then `y` allows once.
    std::thread::sleep(Duration::from_millis(900));
    terminal.send("y");
    expect_all(&terminal, &["You said yes in the terminal", "The invoice is written."]);
    assert_eq!(std::fs::read_to_string(ws.path().join("invoice.txt")).unwrap(), "INV-1");
    dump("full-screen-answered-light", &screen(&terminal));

    terminal.send("\x03");
    let (code, output) = wait_exit(terminal);
    assert_eq!(code, Some(0), "{output}");
    let last_enter = output.rfind("\x1b[?1049h").unwrap();
    let last_leave = output.rfind("\x1b[?1049l").expect("the alternate screen is left");
    assert!(last_leave > last_enter);
}

#[test]
fn typing_while_a_decision_is_open_is_queued_and_esc_never_decides() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let mut terminal = surface(home.path(), ws.path(), WRITE_THEN_REPLY, &["--theme", "dark"]);
    expect_screen(&terminal, "Type a message");
    terminal.send("Write the October invoice\r");
    expect_screen(&terminal, "Waiting for you · Write file invoice.txt");

    // Typing while the box is open goes into the message box; the first key is not a
    // decision key, so the test never races the arm delay (the unit tests cover that race).
    terminal.send("Also cc my accountant");
    expect_screen(&terminal, "Also cc my accountant");
    std::thread::sleep(Duration::from_millis(900));
    terminal.send("\r");
    let parser = expect_all(
        &terminal,
        &["Queued › Also cc my accountant", "Sent after this card is answered"],
    );
    dump("full-screen-queued-dark", &parser);

    // Esc never decides: it switches to typing, and back.
    std::thread::sleep(Duration::from_millis(900));
    terminal.send("\x1b");
    expect_screen(&terminal, "Typing a message: Esc to answer by key.");
    terminal.send("\x1b");
    std::thread::sleep(Duration::from_millis(900));
    assert!(!ws.path().join("invoice.txt").exists(), "nothing was decided");
    assert!(screen(&terminal).screen().contents().contains("Waiting for you · Write file"));

    terminal.send("y");
    expect_screen(&terminal, "You said yes");
    // The queued message goes once the decision is answered and the turn is done.
    expect_screen(&terminal, "Copied your accountant.");
    assert!(ws.path().join("invoice.txt").exists());
    terminal.send("\x03");
    let (code, output) = wait_exit(terminal);
    assert_eq!(code, Some(0), "{output}");
}

#[test]
fn n_denies_with_a_reason_and_nothing_runs() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let mut terminal = surface(home.path(), ws.path(), WRITE_THEN_REPLY, &["--theme", "light"]);
    expect_screen(&terminal, "Type a message");
    terminal.send("Write the October invoice\r");
    expect_screen(&terminal, "Waiting for you · Write file invoice.txt");
    std::thread::sleep(Duration::from_millis(900));
    terminal.send("?");
    let parser = expect_screen(&terminal, "\"content\": \"INV-1\"");
    dump("full-screen-details-light", &parser);
    terminal.send("n");
    expect_screen(&terminal, "Deny — reason (optional) ›");
    terminal.send("not today\r");
    expect_screen(&terminal, "You said no");
    expect_screen(&terminal, "The invoice is written.");
    assert!(!ws.path().join("invoice.txt").exists(), "a denied call never runs");
    terminal.send("\x03");
    assert_eq!(wait_exit(terminal).0, Some(0));
}

#[test]
fn model_text_reaches_the_screen_escaped() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let script = r#"[{"text": "\u001b]0;pwned\u0007\u001b[2Jhello \u202eevil"}]"#;
    let mut terminal = surface(home.path(), ws.path(), script, &["--theme", "dark"]);
    expect_screen(&terminal, "Type a message");
    terminal.send("hi\r");
    // The model's text shows as escapes, and the screen was not cleared.
    expect_all(&terminal, &[r"\u{1b}]0;pwned\u{7}\u{1b}[2Jhello \u{202e}evil", "adk-rust"]);
    struct Title(Vec<u8>);
    impl vt100::Callbacks for Title {
        fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
            self.0 = title.to_vec();
        }
    }
    let mut titled = vt100::Parser::new_with_callbacks(ROWS, COLS, 0, Title(Vec::new()));
    titled.process(&terminal.bytes());
    assert!(titled.callbacks().0.is_empty(), "the model set the window title");
    terminal.send("\x03");
    assert_eq!(wait_exit(terminal).0, Some(0));
}

#[test]
fn full_screen_restores_the_terminal_on_sigterm() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let terminal = surface(home.path(), ws.path(), WRITE_THEN_REPLY, &["--theme", "light"]);
    expect_screen(&terminal, "Type a message");
    let raw = terminal.termios();
    assert_eq!(raw.c_lflag & (libc::ICANON | libc::ECHO), 0, "raw mode while drawn");
    // SAFETY: the child is ours and still running.
    assert_eq!(unsafe { libc::kill(terminal.pid(), libc::SIGTERM) }, 0);
    let restored = {
        let started = Instant::now();
        loop {
            let termios = terminal.termios();
            if termios.c_lflag & libc::ICANON != 0 || started.elapsed() > TIMEOUT {
                break termios;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let (code, output) = wait_exit(terminal);
    assert_eq!(code, Some(143), "{output}");
    assert_ne!(restored.c_lflag & libc::ICANON, 0, "cooked mode again");
    assert_ne!(restored.c_lflag & libc::ECHO, 0, "echo again");
    let last_enter = output.rfind("\x1b[?1049h").unwrap();
    let last_leave = output.rfind("\x1b[?1049l").expect("the alternate screen is left");
    assert!(last_leave > last_enter);
    assert!(output[last_leave..].contains("\x1b[?25h"), "the cursor is shown");
}

#[test]
fn inline_keeps_the_thread_in_the_scrollback() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let mut terminal =
        surface(home.path(), ws.path(), WRITE_THEN_REPLY, &["--ui", "inline", "--theme", "light"]);
    expect_screen(&terminal, "Type a message");
    terminal.send("Write the October invoice\r");
    expect_screen(&terminal, "Waiting for you · Write file invoice.txt");
    std::thread::sleep(Duration::from_millis(900));
    terminal.send("y");
    expect_screen(&terminal, "You said yes in the terminal");
    expect_screen(&terminal, "The invoice is written.");
    assert!(ws.path().join("invoice.txt").exists());
    let parser = screen(&terminal);
    assert!(!parser.screen().alternate_screen(), "inline stays in the main screen");
    dump("inline-light", &parser);
    terminal.send("\x03");
    let (code, output) = wait_exit(terminal);
    assert_eq!(code, Some(0), "{output}");
    assert!(!output.contains("\x1b[?1049h"), "inline never takes the alternate screen");
}

fn json_lines(stdout: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|_| panic!("not JSON: {line:?}")))
        .collect()
}

#[test]
fn json_waits_with_exit_3_while_a_decision_is_open() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let dir = ws.path().display().to_string();
    let args = [
        "--output",
        "json",
        "--message",
        "Write the October invoice",
        "--ephemeral",
        "--dir",
        &dir,
        "--model-script",
        WRITE_THEN_REPLY,
    ];
    let output = run(&args, home.path(), ws.path());
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    let lines = json_lines(&output.stdout);
    let types: Vec<&str> = lines.iter().map(|line| line["type"].as_str().unwrap()).collect();
    assert_eq!(types.first(), Some(&"command"), "{types:?}");
    assert!(types.contains(&"entry"), "{types:?}");
    assert!(types.contains(&"decisions"), "{types:?}");
    let end = lines.last().unwrap();
    assert_eq!(end["type"], "end");
    assert_eq!(end["outcome"], "waitingForYou");
    assert_eq!(end["exitCode"], 3);
    let cards = lines.iter().rev().find(|line| line["type"] == "decisions").unwrap();
    assert_eq!(cards["cards"][0]["heading"], "Write file invoice.txt");
    assert!(!ws.path().join("invoice.txt").exists());
}

#[test]
fn json_exits_0_when_the_turn_is_done_and_1_when_it_fails() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let dir = ws.path().display().to_string();
    let done = run(
        &[
            "--output",
            "json",
            "--message",
            "hello",
            "--ephemeral",
            "--dir",
            &dir,
            "--model-script",
            r#"[{"text": "Hello back."}]"#,
        ],
        home.path(),
        ws.path(),
    );
    assert_eq!(done.status.code(), Some(0), "{}", text(&done));
    let lines = json_lines(&done.stdout);
    assert!(lines.iter().any(|line| {
        line["type"] == "entry"
            && line["entry"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["text"] == "Hello back.")
    }));
    assert_eq!(lines.last().unwrap()["outcome"], "done");

    // A model with nothing to say: the turn fails without a reply.
    let failed = run(
        &[
            "--output",
            "json",
            "--message",
            "hello",
            "--ephemeral",
            "--dir",
            &dir,
            "--model-script",
            "[]",
        ],
        home.path(),
        ws.path(),
    );
    assert_eq!(failed.status.code(), Some(1), "{}", text(&failed));
    assert_eq!(json_lines(&failed.stdout).last().unwrap()["outcome"], "error");
}

/// The device tokens `--pair` kept under `dir`.
fn token_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(token_files(&path));
        } else if path.extension().is_some_and(|ext| ext == "token")
            && path.parent().is_some_and(|parent| parent.ends_with("adk-rust/devices"))
        {
            found.push(path);
        }
    }
    found
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// `adk-rust up` at `kernel` on `port` with a scripted model, listening once it returns. Its
/// HTTP control plane needs a token even from loopback, so a device token is what admits the
/// remote terminal.
fn kernel_up(home: &Path, kernel: &Path, ws: &Path, port: u16, script: &str) -> Terminal {
    let bind = format!("127.0.0.1:{port}");
    let (kernel, dir) = (kernel.display().to_string(), ws.display().to_string());
    let args = [
        "up",
        "--home",
        &kernel,
        "--bind",
        &bind,
        "--dir",
        &dir,
        "--model-script",
        script,
        "--mode",
        "ask",
        "--auth-token",
        "operator-only",
    ];
    let terminal = Terminal::spawn_in(&args, home, ws);
    let started = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(started.elapsed() < TIMEOUT, "up did not listen:\n{}", terminal.text());
        std::thread::sleep(Duration::from_millis(100));
    }
    terminal
}

/// The terminal attaches to a running kernel: JSON lines on its local socket, then, paired by
/// a code as a phone is, the full screen over HTTP with a device token, answering by key
/// (spec 3.3 §5.2, §10 K2).
#[test]
fn the_terminal_attaches_to_a_kernel_locally_and_paired_over_http() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let kernel = home.path().join("kernel");
    let port = free_port();
    let mut up = kernel_up(home.path(), &kernel, ws.path(), port, WRITE_THEN_REPLY);
    let kernel_arg = kernel.display().to_string();

    // On the kernel's socket: the turn runs in the kernel, and the run waits for a decision.
    let json = run(
        &["--home", &kernel_arg, "--output", "json", "--message", "Write the October invoice"],
        home.path(),
        ws.path(),
    );
    assert_eq!(json.status.code(), Some(3), "{}", text(&json));
    let lines = json_lines(&json.stdout);
    let cards = lines.iter().rev().find(|line| line["type"] == "decisions").unwrap();
    assert_eq!(cards["cards"][0]["heading"], "Write file invoice.txt", "{lines:?}");
    assert!(!ws.path().join("invoice.txt").exists());

    // Without a token, the kernel's HTTP control plane admits nothing.
    let server = format!("http://127.0.0.1:{port}");
    let refused = run(&["--server", &server, "--output", "json"], home.path(), ws.path());
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused));

    // `adk-rust pair` on the kernel's machine shows a code; the terminal claims it.
    let paired = run(&["pair", "--home", &kernel_arg, "--name", "laptop"], home.path(), ws.path());
    assert!(paired.status.success(), "{}", text(&paired));
    let shown = String::from_utf8_lossy(&paired.stdout).into_owned();
    let code = shown
        .lines()
        .find_map(|line| line.strip_prefix("Pairing code"))
        .map(|code| code.trim().to_string())
        .unwrap_or_else(|| panic!("no code in:\n{shown}"));
    let mut remote = Terminal::spawn_sized_env(
        &["--server", &server, "--pair", &code, "--theme", "dark"],
        home.path(),
        ws.path(),
        ROWS,
        COLS,
        &[("COLORTERM", "truecolor")],
    );
    // The status line names the kernel.
    let parser = expect_all(&remote, &["Waiting for you · Write file invoice.txt", &server]);
    dump("kernel-paired-full-screen-dark", &parser);
    std::thread::sleep(Duration::from_millis(900));
    remote.send("y");
    expect_screen(&remote, "You said yes");
    let started = Instant::now();
    while !ws.path().join("invoice.txt").exists() {
        assert!(started.elapsed() < TIMEOUT, "the kernel did not run the allowed call");
        std::thread::sleep(Duration::from_millis(100));
    }
    remote.send("\x03");
    let (code_left, output) = remote.wait();
    assert_eq!(code_left, Some(0), "{output}");
    // The name given when the code was issued, not the one the terminal proposed.
    assert!(output.contains("Paired as laptop (decide, read, stop)"), "{output}");

    // The device token was kept, owner-only, and the next start needs no code.
    let kept = token_files(home.path());
    assert_eq!(kept.len(), 1, "{kept:?}");
    let mode = std::os::unix::fs::PermissionsExt::mode(&kept[0].metadata().unwrap().permissions());
    assert_eq!(mode & 0o777, 0o600, "the token is readable by its owner only");
    assert!(std::fs::read_to_string(&kept[0]).unwrap().starts_with("adkd_"));
    let again = run(&["--server", &server, "--output", "json"], home.path(), ws.path());
    assert_eq!(again.status.code(), Some(0), "{}", text(&again));
    let listed = run(&["pair", "--home", &kernel_arg, "list"], home.path(), ws.path());
    assert!(text(&listed).contains("laptop"), "{}", text(&listed));

    // Stop the kernel before its temporary home goes: dropping the handle stops nothing.
    up.send("\x03");
    let (stopped, output) = up.wait();
    assert!(stopped.is_some(), "the kernel stopped:\n{output}");
}

/// Every screen of the terminal in light and dark, for the PR's screenshots: written only
/// when `$ADK_TUI_SCREENS` names a directory, in 24-bit colour.
#[test]
fn every_screen_in_light_and_dark() {
    let Ok(dir) = std::env::var("ADK_TUI_SCREENS") else {
        return;
    };
    for theme in ["light", "dark"] {
        let home = tempfile::tempdir().unwrap();
        let ws = workspace();
        let shot =
            |name: &str, terminal: &Terminal| dump(&format!("{name}-{theme}"), &screen(terminal));
        let dir_arg = ws.path().display().to_string();
        let args = [
            "--ephemeral",
            "--dir",
            dir_arg.as_str(),
            "--model-script",
            WRITE_THEN_REPLY,
            "--mode",
            "ask",
            "--theme",
            theme,
        ];
        let truecolor = [("COLORTERM", "truecolor")];
        let mut full =
            Terminal::spawn_sized_env(&args, home.path(), ws.path(), ROWS, COLS, &truecolor);
        expect_screen(&full, "Type a message");
        shot("1-start", &full);
        full.send("Write the October invoice\r");
        expect_screen(&full, "Waiting for you · Write file invoice.txt");
        std::thread::sleep(Duration::from_millis(900));
        shot("2-decision", &full);
        full.send("?");
        expect_screen(&full, "\"content\": \"INV-1\"");
        shot("3-details", &full);
        // Esc closes the details, then switches to typing: a key no longer answers.
        full.send("\x1b");
        std::thread::sleep(Duration::from_millis(200));
        full.send("\x1b");
        expect_screen(&full, "Typing a message: Esc to answer by key.");
        full.send("also cc my accountant");
        expect_screen(&full, "also cc my accountant");
        std::thread::sleep(Duration::from_millis(900));
        full.send("\r");
        expect_screen(&full, "Queued › also cc my accountant");
        shot("4-queued", &full);
        full.send("\x14");
        std::thread::sleep(Duration::from_millis(500));
        shot("5-today", &full);
        full.send("\x1b");
        full.send("\x12");
        expect_screen(&full, "What your agents can do");
        shot("6-permissions", &full);
        full.send("\x1b");
        full.send("/help\r");
        expect_screen(&full, "Never decides: switch");
        shot("7-help", &full);
        full.send("\x1b");
        std::thread::sleep(Duration::from_millis(200));
        // Back to answering by key.
        full.send("\x1b");
        std::thread::sleep(Duration::from_millis(900));
        full.send("y");
        expect_screen(&full, "Copied your accountant.");
        shot("8-answered", &full);
        full.send("\x03");
        full.wait();

        let mut inline_args = args.to_vec();
        inline_args.extend(["--ui", "inline"]);
        let mut inline =
            Terminal::spawn_sized_env(&inline_args, home.path(), ws.path(), ROWS, COLS, &truecolor);
        expect_screen(&inline, "Type a message");
        inline.send("Write the October invoice\r");
        expect_screen(&inline, "Waiting for you · Write file invoice.txt");
        std::thread::sleep(Duration::from_millis(900));
        inline.send("y");
        expect_screen(&inline, "The invoice is written.");
        shot("9-inline", &inline);
        inline.send("\x03");
        inline.wait();

        let output = run(
            &[
                "--output",
                "json",
                "--message",
                "Write the October invoice",
                "--ephemeral",
                "--dir",
                &dir_arg,
                "--model-script",
                WRITE_THEN_REPLY,
            ],
            home.path(),
            ws.path(),
        );
        assert_eq!(output.status.code(), Some(3), "{}", text(&output));
        let mut parser = vt100::Parser::new(ROWS, COLS, 0);
        let shown = format!(
            "$ adk-rust --output json --message 'Write the October invoice'; echo \"exit $?\"\r\n{}exit 3\r\n",
            String::from_utf8_lossy(&output.stdout).replace('\n', "\r\n")
        );
        parser.process(shown.as_bytes());
        dump(&format!("10-json-{theme}"), &parser);
    }
    assert!(Path::new(&dir).join("2-decision-dark.html").exists());
}

/// One live turn on a real model: the full screen holds its write as a decision, `y` lets it
/// run. Needs `ANTHROPIC_API_KEY`; two or three model calls.
#[test]
#[ignore = "needs ANTHROPIC_API_KEY and calls the model"]
fn live_model_write_is_decided_by_key() {
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let dir = ws.path().display().to_string();
    let args = [
        "--provider",
        "anthropic",
        "--model",
        "claude-sonnet-5-5",
        "--ephemeral",
        "--dir",
        &dir,
        "--mode",
        "ask",
        "--theme",
        "dark",
    ];
    let mut terminal = Terminal::spawn_sized_env(
        &args,
        home.path(),
        ws.path(),
        ROWS,
        COLS,
        &[("COLORTERM", "truecolor")],
    );
    expect_screen(&terminal, "Type a message");
    terminal.send("Create the file notes.txt containing exactly the word hello. Nothing else.\r");
    let parser = expect_screen(&terminal, "Waiting for you ·");
    assert!(!ws.path().join("notes.txt").exists(), "held, not written");
    dump("live-decision-dark", &parser);
    std::thread::sleep(Duration::from_millis(900));
    terminal.send("y");
    expect_screen(&terminal, "You said yes");
    let started = Instant::now();
    while !ws.path().join("notes.txt").exists() {
        assert!(started.elapsed() < TIMEOUT, "the allowed write did not run");
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_secs(5));
    dump("live-answered-dark", &screen(&terminal));
    assert!(std::fs::read_to_string(ws.path().join("notes.txt")).unwrap().contains("hello"));
    terminal.send("\x03");
    assert_eq!(terminal.wait().0, Some(0));
}

// ---------------------------------------------------------------------------
// Review of #1041: each test fails on the branch before its fix.
// ---------------------------------------------------------------------------

/// Waits until the pseudo-terminal is back in cooked mode, or the timeout; returns its
/// settings.
fn cooked(terminal: &Terminal) -> libc::termios {
    let started = Instant::now();
    loop {
        let termios = terminal.termios();
        if termios.c_lflag & libc::ICANON != 0 || started.elapsed() > TIMEOUT {
            return termios;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Sends `signal` to the binary.
fn signal(terminal: &Terminal, signal: i32) {
    // SAFETY: the child is ours and still running.
    assert_eq!(unsafe { libc::kill(terminal.pid(), signal) }, 0);
}

/// Piped into inline, the line the run ends with names the held call through
/// `terminal_safe`: a model's escape sequences never reach the terminal (S-I9).
#[test]
fn piped_inline_escapes_the_line_it_ends_with() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let dir = ws.path().display().to_string();
    let script = r#"[{"call": {"name": "write_file", "args": {"path": "\u001b]0;owned\u0007\u001b[2Jnotes.txt", "content": "x"}}}]"#;
    let output = common::run_input(
        &[
            "--ui",
            "inline",
            "--ephemeral",
            "--dir",
            &dir,
            "--model-script",
            script,
            "--mode",
            "ask",
        ],
        home.path(),
        ws.path(),
        "tidy my notes\n",
    );
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Waiting for you"), "{stdout}");
    assert!(!output.stdout.contains(&0x1b), "an escape sequence reached the terminal: {stdout:?}");
}

/// Inline turns bracketed paste on, so a paste reaches the message box as a paste and never
/// answers a decision (§7.4 rule 2, S-I4), and turns it off again.
#[test]
fn inline_takes_a_paste_as_a_paste() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let mut terminal =
        surface(home.path(), ws.path(), WRITE_THEN_REPLY, &["--ui", "inline", "--theme", "light"]);
    expect_screen(&terminal, "Type a message");
    terminal.send("Write the October invoice\r");
    expect_screen(&terminal, "Waiting for you · Write file invoice.txt");
    std::thread::sleep(Duration::from_millis(900));
    terminal.send("\x1b[200~always use tabs\x1b[201~");
    expect_screen(&terminal, "always use tabs");
    std::thread::sleep(Duration::from_millis(500));
    assert!(!ws.path().join("invoice.txt").exists(), "the paste answered the decision");
    terminal.send("\x03");
    std::thread::sleep(Duration::from_millis(200));
    terminal.send("\x03");
    let (code, output) = wait_exit(terminal);
    assert_eq!(code, Some(0), "{output}");
    let on = output.find("\x1b[?2004h").expect("bracketed paste is turned on");
    let off = output.rfind("\x1b[?2004l").expect("bracketed paste is turned off");
    assert!(off > on);
}

/// `--continue` opens the latest session of the workspace, as the REPL does.
#[test]
fn continue_opens_the_latest_session_of_the_workspace() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let dir = ws.path().display().to_string();
    let first = run(
        &[
            "--output",
            "json",
            "--message",
            "hello",
            "--dir",
            &dir,
            "--model-script",
            r#"[{"text": "Hello."}]"#,
        ],
        home.path(),
        ws.path(),
    );
    assert_eq!(first.status.code(), Some(0), "{}", text(&first));
    let session = json_lines(&first.stdout).last().unwrap()["sessionId"].clone();
    let again = run(
        &[
            "--continue",
            "--output",
            "json",
            "--message",
            "again",
            "--dir",
            &dir,
            "--model-script",
            r#"[{"text": "Again."}]"#,
        ],
        home.path(),
        ws.path(),
    );
    assert_eq!(again.status.code(), Some(0), "{}", text(&again));
    assert_eq!(json_lines(&again.stdout).last().unwrap()["sessionId"], session);
}

/// A session that works in another workspace is refused, as the REPL and `code` refuse it.
#[test]
fn a_session_of_another_workspace_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let other = workspace();
    let (dir, other_dir) = (ws.path().display().to_string(), other.path().display().to_string());
    let pinned = run(
        &[
            "code",
            "hi",
            "--session",
            "pinned",
            "--dir",
            &dir,
            "--model-script",
            r#"[{"text": "Hi."}, {"text": "{\"facts\": []}"}]"#,
        ],
        home.path(),
        ws.path(),
    );
    assert!(pinned.status.success(), "{}", text(&pinned));
    let refused = run(
        &[
            "--session",
            "pinned",
            "--dir",
            &other_dir,
            "--output",
            "json",
            "--message",
            "hello",
            "--model-script",
            r#"[{"text": "Hello."}]"#,
        ],
        home.path(),
        other.path(),
    );
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused));
    assert!(text(&refused).contains("session pinned works in"), "{}", text(&refused));
}

/// A signal that arrives as the full screen starts, before its first frame, still gives the
/// terminal back.
#[test]
fn a_signal_as_the_full_screen_starts_restores_the_terminal() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let terminal = surface(home.path(), ws.path(), WRITE_THEN_REPLY, &["--theme", "light"]);
    let started = Instant::now();
    while !terminal.bytes().windows(8).any(|bytes| bytes == b"\x1b[?1049h") {
        assert!(started.elapsed() < TIMEOUT, "the full screen did not start");
        std::thread::sleep(Duration::from_millis(1));
    }
    signal(&terminal, libc::SIGTERM);
    let restored = cooked(&terminal);
    let (code, output) = wait_exit(terminal);
    assert_eq!(code, Some(143), "{output}");
    assert_ne!(restored.c_lflag & libc::ICANON, 0, "cooked mode again");
    assert_ne!(restored.c_lflag & libc::ECHO, 0, "echo again");
}

/// Inline gives the terminal back on a signal too.
#[test]
fn inline_restores_the_terminal_on_sigterm() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let terminal =
        surface(home.path(), ws.path(), WRITE_THEN_REPLY, &["--ui", "inline", "--theme", "light"]);
    expect_screen(&terminal, "Type a message");
    assert_eq!(terminal.termios().c_lflag & (libc::ICANON | libc::ECHO), 0, "raw mode");
    signal(&terminal, libc::SIGTERM);
    let restored = cooked(&terminal);
    let (code, output) = wait_exit(terminal);
    assert_eq!(code, Some(143), "{output}");
    assert_ne!(restored.c_lflag & libc::ICANON, 0, "cooked mode again");
    assert_ne!(restored.c_lflag & libc::ECHO, 0, "echo again");
}

/// What a tool server writes to standard error goes to `terminal.log`, never over the full
/// screen, and never to the terminal unescaped.
#[test]
fn a_tool_servers_stderr_never_reaches_the_full_screen() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let dir = ws.path().display().to_string();
    std::fs::write(
        ws.path().join(".mcp.json"),
        r#"{"mcpServers": {"noisy": {"command": "sh", "args": ["-c", "echo NOISY-SERVER-STDERR >&2; sleep 2"]}}}"#,
    )
    .unwrap();
    // Trust the workspace, so its servers start: `--trust` records trust for a workspace
    // with instruction files.
    std::fs::write(ws.path().join("AGENTS.md"), "Answer briefly.\n").unwrap();
    run(
        &[
            "code",
            "hi",
            "--trust",
            "--dir",
            &dir,
            "--model-script",
            r#"[{"text": "Hi."}, {"text": "{\"facts\": []}"}]"#,
        ],
        home.path(),
        ws.path(),
    );
    let mut terminal =
        surface(home.path(), ws.path(), r#"[{"text": "Hello back."}]"#, &["--theme", "light"]);
    expect_screen(&terminal, "Type a message");
    terminal.send("hello\r");
    expect_screen(&terminal, "Hello back.");
    // `terminal.log` is in the platform's config directory under the test's home.
    let started = Instant::now();
    let log = loop {
        let log = find_file(home.path(), "terminal.log")
            .map(|path| std::fs::read_to_string(path).unwrap())
            .unwrap_or_default();
        if log.contains("NOISY-SERVER-STDERR") || started.elapsed() > Duration::from_secs(10) {
            break log;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let shown = String::from_utf8_lossy(&terminal.bytes()).into_owned();
    terminal.send("\x03");
    let _ = wait_exit(terminal);
    assert!(!shown.contains("NOISY-SERVER-STDERR"), "a server wrote over the full screen");
    assert!(log.contains("NOISY-SERVER-STDERR"), "the server started and wrote:\n{log}");
}

/// The first file named `name` under `dir`.
fn find_file(dir: &Path, name: &str) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|file| file == name) {
            return Some(path);
        }
    }
    None
}

/// The terminal in process keeps long-term memory, as the REPL it replaces does.
#[test]
fn the_terminal_in_process_keeps_long_term_memory() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let dir = ws.path().display().to_string();
    let script = r#"[
        {"call": {"name": "memory_remember", "args": {"text": "Use pnpm, not npm", "kind": "preference"}}},
        {"text": "Noted."}
    ]"#;
    let told = run(
        &[
            "--output",
            "json",
            "--message",
            "From now on use pnpm, not npm",
            "--dir",
            &dir,
            "--mode",
            "ask",
            "--model-script",
            script,
        ],
        home.path(),
        ws.path(),
    );
    assert_eq!(told.status.code(), Some(0), "{}", text(&told));
    let listed = run(&["memory", "list", "--dir", &dir], home.path(), ws.path());
    assert!(text(&listed).contains("Use pnpm, not npm"), "{}", text(&listed));
}

/// `--yes` approves held calls in the terminal as it does in the REPL.
#[test]
fn yes_approves_held_calls() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let dir = ws.path().display().to_string();
    let output = run(
        &[
            "--output",
            "json",
            "--message",
            "Write the October invoice",
            "--ephemeral",
            "--yes",
            "--dir",
            &dir,
            "--model-script",
            WRITE_THEN_REPLY,
        ],
        home.path(),
        ws.path(),
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(std::fs::read_to_string(ws.path().join("invoice.txt")).unwrap(), "INV-1");
}

/// An untrusted workspace with instruction files says so; `--trust` trusts it and the note
/// goes, on that run and the next.
#[test]
fn an_untrusted_workspace_says_so_and_trust_loads_it() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    std::fs::write(ws.path().join("AGENTS.md"), "Answer briefly.\n").unwrap();
    let dir = ws.path().display().to_string();
    let reply = r#"[{"text": "Hello back."}]"#;
    let args = |trust: bool| {
        let mut args =
            vec!["--ui", "inline", "--ephemeral", "--dir", dir.as_str(), "--model-script", reply];
        if trust {
            args.push("--trust");
        }
        args
    };
    let untrusted = common::run_input(&args(false), home.path(), ws.path(), "hello\n");
    assert!(text(&untrusted).contains("--trust"), "{}", text(&untrusted));
    let trusted = common::run_input(&args(true), home.path(), ws.path(), "hello\n");
    assert!(!text(&trusted).contains("not trusted"), "{}", text(&trusted));
    let next = common::run_input(&args(false), home.path(), ws.path(), "hello\n");
    assert!(!text(&next).contains("not trusted"), "{}", text(&next));
}

/// `--server`, `--home` and `--token` before a subcommand are refused, never applied to the
/// local kernel instead of the named one.
#[test]
fn kernel_flags_before_a_subcommand_are_refused() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let output =
        run(&["--server", "http://127.0.0.1:9", "--token", "t", "freeze"], home.path(), ws.path());
    assert_ne!(output.status.code(), Some(0), "{}", text(&output));
    assert!(text(&output).contains("after the subcommand"), "{}", text(&output));
}

/// On a terminal that names itself `dumb`, the terminal stays in cooked mode and writes plain
/// lines: no escape sequences at all.
#[test]
fn a_dumb_terminal_gets_plain_lines() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let dir = ws.path().display().to_string();
    let mut terminal = Terminal::spawn_sized_env(
        &["--ephemeral", "--dir", &dir, "--model-script", r#"[{"text": "Hello back."}]"#],
        home.path(),
        ws.path(),
        ROWS,
        COLS,
        &[("TERM", "dumb")],
    );
    std::thread::sleep(Duration::from_millis(500));
    assert_ne!(terminal.termios().c_lflag & libc::ICANON, 0, "cooked mode");
    terminal.send("hello\r");
    let started = Instant::now();
    while !terminal.text().contains("Hello back.") {
        assert!(started.elapsed() < TIMEOUT, "no reply:\n{}", terminal.text());
        std::thread::sleep(Duration::from_millis(30));
    }
    terminal.send("\x04");
    let (code, output) = wait_exit(terminal);
    assert_eq!(code, Some(0), "{output}");
    assert!(!output.contains('\x1b'), "an escape sequence on a dumb terminal: {output:?}");
}
