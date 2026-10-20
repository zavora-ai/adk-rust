//! `adk-rust chat` in a pseudo-terminal: held calls are shown and answered at the console
//! (spec 3.1 §8.1, §11 adk-cli row). The model is scripted, so no provider key is needed.

#![cfg(all(unix, feature = "scripted-model"))]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(60);

const RECORD_THEN_ANSWER: &str = r#"[
    {"call": {"name": "record_note", "args": {"note": "ship it"}}},
    {"text": "Done with the note."}
]"#;

/// The binary running on the slave side of a pseudo-terminal.
struct Terminal {
    master: File,
    child: Child,
    output: Arc<Mutex<Vec<u8>>>,
    /// How much output earlier expectations consumed.
    seen: usize,
}

impl Terminal {
    fn spawn(args: &[&str], home: &Path) -> Self {
        let (mut master_fd, mut slave_fd) = (0, 0);
        let mut size = libc::winsize { ws_row: 50, ws_col: 220, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: openpty writes two descriptors into the provided integers.
        let opened = unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        };
        assert_eq!(opened, 0, "openpty: {}", std::io::Error::last_os_error());
        // SAFETY: both descriptors were just opened and are owned here.
        let (master, slave) =
            unsafe { (File::from_raw_fd(master_fd), File::from_raw_fd(slave_fd)) };

        let mut command = Command::new(env!("CARGO_BIN_EXE_adk-rust"));
        command
            .args(args)
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("USER", "pty-tester")
            .env("TERM", "xterm")
            .env("RUST_LOG", "off")
            // `e` falls back to one typed line instead of starting an editor.
            .env_remove("VISUAL")
            .env_remove("EDITOR")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        // SAFETY: only async-signal-safe calls run between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().expect("adk-rust starts");
        drop(command);

        let output = Arc::new(Mutex::new(Vec::new()));
        let mut reader = master.try_clone().unwrap();
        let mut answerer = master.try_clone().unwrap();
        let sink = output.clone();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            // EIO once the child closes its side ends the loop.
            while let Ok(read) = reader.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                let chunk = &buffer[..read];
                // Answer the editor's cursor-position query, as a terminal would.
                if chunk.windows(4).any(|window| window == b"\x1b[6n") {
                    let _ = answerer.write_all(b"\x1b[1;1R");
                }
                sink.lock().unwrap().extend_from_slice(chunk);
            }
        });
        Self { master, child, output, seen: 0 }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }

    /// Waits for `needle` in the output not yet consumed, then consumes through it.
    fn expect(&mut self, needle: &str) {
        let started = Instant::now();
        loop {
            let text = self.text();
            if let Some(at) = text[self.seen.min(text.len())..].find(needle) {
                self.seen += at + needle.len();
                return;
            }
            assert!(started.elapsed() < TIMEOUT, "timed out waiting for {needle:?} in:\n{text}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn send(&mut self, keys: &str) {
        self.master.write_all(keys.as_bytes()).unwrap();
        self.master.flush().unwrap();
    }

    fn finish(mut self) -> String {
        self.send("exit\r");
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "adk-rust exited with {status}:\n{}", self.text());
                break;
            }
            if started.elapsed() > TIMEOUT {
                let _ = self.child.kill();
                panic!("adk-rust did not exit:\n{}", self.text());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.text()
    }
}

fn chat(home: &Path, script: &str) -> Terminal {
    Terminal::spawn(&["chat", "--ephemeral", "--approval-fixture", "--model-script", script], home)
}

#[test]
fn a_held_call_is_shown_and_y_runs_it() {
    let home = tempfile::tempdir().unwrap();
    let mut terminal = chat(home.path(), RECORD_THEN_ANSWER);
    terminal.expect("You >");
    terminal.send("please record a note\r");
    terminal.expect(r#"[approval needed] record_note {"note":"ship it"}"#);
    terminal.expect("[y] allow once  [n] deny (reason)  [e] edit args  [?] details >");
    terminal.send("y\r");
    terminal.expect("[approved] record_note");
    terminal.expect(r#"[tool-response] record_note {"recorded":"ship it"}"#);
    terminal.expect("Done with the note.");
    terminal.expect("You >");
    terminal.finish();
}

#[test]
fn n_denies_the_call_and_the_model_hears_the_reason() {
    let home = tempfile::tempdir().unwrap();
    let mut terminal = chat(home.path(), RECORD_THEN_ANSWER);
    terminal.expect("You >");
    terminal.send("please record a note\r");
    terminal.expect("[approval needed] record_note");
    terminal.expect("details >");
    terminal.send("n\r");
    terminal.expect("reason (optional) >");
    terminal.send("not today\r");
    terminal.expect("[denied] record_note");
    terminal.expect("the approver declined it: not today");
    terminal.expect("Done with the note.");
    let output = terminal.finish();
    assert!(!output.contains(r#""recorded""#), "the denied call ran:\n{output}");
}

#[test]
fn e_edits_the_arguments_and_y_runs_the_edited_call() {
    let home = tempfile::tempdir().unwrap();
    let mut terminal = chat(home.path(), RECORD_THEN_ANSWER);
    terminal.expect("You >");
    terminal.send("please record a note\r");
    terminal.expect("[approval needed] record_note");
    terminal.expect("details >");
    terminal.send("e\r");
    terminal.expect("arguments (JSON) >");
    terminal.send("{\"note\": \"ship it tomorrow\"}\r");
    terminal.expect(r#"edited: record_note {"note":"ship it tomorrow"}"#);
    terminal.expect("details >");
    terminal.send("y\r");
    terminal.expect("[approved] record_note");
    terminal.expect(r#"[tool-response] record_note {"recorded":"ship it tomorrow"}"#);
    let output = terminal.finish();
    assert!(!output.contains(r#"{"recorded":"ship it"}"#), "the original call ran:\n{output}");
}

#[test]
fn ctrl_c_at_the_prompt_denies_and_cancels_the_run() {
    let home = tempfile::tempdir().unwrap();
    let mut terminal = chat(home.path(), RECORD_THEN_ANSWER);
    terminal.expect("You >");
    terminal.send("please record a note\r");
    terminal.expect("[approval needed] record_note");
    terminal.expect("[y] allow once");
    terminal.send("\x03");
    terminal.expect("denied; the run is interrupted");
    terminal.expect("Interrupted.");
    // The REPL reads again from the same reader: nothing was left blocking on stdin.
    terminal.expect("You >");
    let output = terminal.finish();
    assert!(!output.contains(r#""recorded""#), "the call ran:\n{output}");
    assert!(!output.contains("Done with the note."), "the run continued:\n{output}");
    assert!(output.contains("Goodbye."), "{output}");
}

#[test]
fn without_a_terminal_the_held_call_is_denied() {
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_adk-rust"))
        .args(["chat", "--ephemeral", "--approval-fixture", "--model-script", RECORD_THEN_ANSWER])
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("RUST_LOG", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"please record a note\ny\n").unwrap();
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("there is no terminal to ask"), "{stdout}");
    assert!(stdout.contains("[denied] record_note"), "{stdout}");
    assert!(!stdout.contains(r#""recorded""#), "the call ran without approval:\n{stdout}");
}

#[cfg(feature = "persist")]
#[test]
fn continue_resumes_the_latest_session() {
    let home = tempfile::tempdir().unwrap();
    let script = r#"[{"text": "First answer."}]"#;
    let mut first =
        Terminal::spawn(&["chat", "--session", "notes", "--model-script", script], home.path());
    first.expect("Session  : notes");
    first.expect("You >");
    first.send("hello\r");
    first.expect("First answer.");
    first.finish();

    let mut resumed =
        Terminal::spawn(&["chat", "--continue", "--model-script", script], home.path());
    resumed.expect("Session  : notes");
    resumed.finish();

    let mut fresh = Terminal::spawn(&["chat", "--model-script", script], home.path());
    fresh.expect("Session  : ");
    let output = fresh.finish();
    assert!(!output.contains("Session  : notes"), "{output}");
}
