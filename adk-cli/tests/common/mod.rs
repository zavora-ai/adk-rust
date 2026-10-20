//! A pseudo-terminal driver and a plain runner for `adk-rust` binary tests.

// Each test crate uses a different subset of these helpers.
#![allow(dead_code)]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const TIMEOUT: Duration = Duration::from_secs(60);

/// Points `command` at a private home and config directory, with logging off.
pub fn isolate(command: &mut Command, home: &Path) {
    command
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("USER", "pty-tester")
        .env("TERM", "xterm")
        .env("RUST_LOG", "off")
        .env_remove("ADK_SERVE_TOKEN")
        // `e` falls back to one typed line instead of starting an editor.
        .env_remove("VISUAL")
        .env_remove("EDITOR");
}

/// Runs the binary to completion without a terminal: stdin is closed.
pub fn run(args: &[&str], home: &Path, cwd: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_adk-rust"));
    command.args(args).current_dir(cwd).stdin(Stdio::null());
    isolate(&mut command, home);
    command.output().expect("adk-rust runs")
}

/// Standard output and error of `output`, joined.
pub fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The binary running on the slave side of a pseudo-terminal.
pub struct Terminal {
    master: File,
    child: Child,
    output: Arc<Mutex<Vec<u8>>>,
    /// How much output earlier expectations consumed.
    seen: usize,
}

impl Terminal {
    pub fn spawn(args: &[&str], home: &Path) -> Self {
        Self::spawn_in(args, home, home)
    }

    pub fn spawn_in(args: &[&str], home: &Path, cwd: &Path) -> Self {
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
        isolate(&mut command, home);
        command
            .args(args)
            .current_dir(cwd)
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

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }

    /// Waits for `needle` in the output not yet consumed, then consumes through it.
    pub fn expect(&mut self, needle: &str) {
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

    pub fn send(&mut self, keys: &str) {
        self.master.write_all(keys.as_bytes()).unwrap();
        self.master.flush().unwrap();
    }

    pub fn finish(mut self) -> String {
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
