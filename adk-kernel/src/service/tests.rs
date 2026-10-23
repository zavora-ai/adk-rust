//! Service definitions rendered for each platform, on every platform (spec 3.2 §7.12, §10
//! W3): the golden output `adk-rust up --install --print` shows.

use std::path::Path;

use super::{ServiceDefinition, ServicePlatform};

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_string()).collect()
}

fn unix_definition(home: &str) -> ServiceDefinition {
    ServiceDefinition {
        name: "adk-rust-kernel-0123456789abcdef".into(),
        program: "/Users/ana/.cargo/bin/adk-rust".into(),
        args: strings(&["up", "--mode", "ask", "--home", home, "--dir", "/Users/ana/my project"]),
        home: home.into(),
        working_directory: "/Users/ana/my project".into(),
        environment: vec![("PATH".into(), "/Users/ana/.cargo/bin:/usr/bin:/bin".into())],
        account: None,
    }
}

#[test]
fn the_launchd_agent_runs_up_as_the_user_logs_to_the_home_and_restarts_on_failure() {
    let definition = unix_definition("/Users/ana/Library/Application Support/adk-rust");
    let expected = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>adk-rust-kernel-0123456789abcdef</string>
    <key>ProgramArguments</key>
    <array>
        <string>/Users/ana/.cargo/bin/adk-rust</string>
        <string>up</string>
        <string>--mode</string>
        <string>ask</string>
        <string>--home</string>
        <string>/Users/ana/Library/Application Support/adk-rust</string>
        <string>--dir</string>
        <string>/Users/ana/my project</string>
    </array>
    <key>WorkingDirectory</key>
    <string>/Users/ana/my project</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/Users/ana/.cargo/bin:/usr/bin:/bin</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>ExitTimeOut</key>
    <integer>60</integer>
    <key>Umask</key>
    <integer>63</integer>
    <key>StandardOutPath</key>
    <string>/Users/ana/Library/Application Support/adk-rust/kernel.log</string>
    <key>StandardErrorPath</key>
    <string>/Users/ana/Library/Application Support/adk-rust/kernel.log</string>
</dict>
</plist>
"#;
    assert_eq!(definition.render(ServicePlatform::Launchd), expected);
}

#[test]
fn the_launchd_plist_escapes_what_xml_reserves() {
    let definition =
        unix_definition("/tmp/a&b").with_args(["up", "--instruction", "<be \"terse\">"]);
    let plist = definition.render(ServicePlatform::Launchd);
    assert!(plist.contains("<string>&lt;be &quot;terse&quot;&gt;</string>"), "{plist}");
    assert!(plist.contains("<string>/tmp/a&amp;b/kernel.log</string>"), "{plist}");
}

#[test]
fn the_systemd_user_unit_runs_up_logs_to_the_home_and_restarts_on_failure() {
    let definition = unix_definition("/home/ana/.config/adk-rust");
    let expected = r#"[Unit]
Description=ADK-Rust agent kernel (/home/ana/.config/adk-rust)

[Service]
Type=simple
ExecStart=/Users/ana/.cargo/bin/adk-rust up --mode ask --home /home/ana/.config/adk-rust --dir "/Users/ana/my project"
WorkingDirectory=/Users/ana/my project
Environment="PATH=/Users/ana/.cargo/bin:/usr/bin:/bin"
UMask=0077
Restart=on-failure
RestartSec=5
TimeoutStopSec=60
StandardOutput=append:/home/ana/.config/adk-rust/kernel.log
StandardError=inherit

[Install]
WantedBy=default.target
"#;
    assert_eq!(definition.render(ServicePlatform::Systemd), expected);
}

#[test]
fn systemd_words_escape_specifiers_variables_and_quotes() {
    let definition = unix_definition("/srv/50%").with_args([
        "up",
        "--instruction",
        "cost in $ and 100%",
        "--model",
        "a\"b\\c",
        "",
        "plain",
    ]);
    let unit = definition.render(ServicePlatform::Systemd);
    assert!(
        unit.contains(
            r#"ExecStart=/Users/ana/.cargo/bin/adk-rust up --instruction "cost in $$ and 100%%" --model "a\"b\\c" "" plain"#
        ),
        "{unit}"
    );
    assert!(unit.contains("Description=ADK-Rust agent kernel (/srv/50%%)\n"), "{unit}");
    assert!(unit.contains("StandardOutput=append:/srv/50%%/kernel.log\n"), "{unit}");
}

fn windows_definition() -> ServiceDefinition {
    let home = r"C:\Users\ana\AppData\Roaming\adk-rust";
    ServiceDefinition {
        name: "adk-rust-kernel-0123456789abcdef".into(),
        program: r"C:\Program Files\adk-rust\adk-rust.exe".into(),
        args: strings(&[
            "up",
            "--home",
            home,
            "--dir",
            r"C:\Users\ana\my project\",
            "--windows-service",
            "adk-rust-kernel-0123456789abcdef",
        ]),
        home: home.into(),
        working_directory: r"C:\Users\ana\my project".into(),
        environment: vec![("PATH".into(), r"C:\Users\ana\.cargo\bin;C:\Windows\system32".into())],
        account: Some(r"DESKTOP-1\ana".into()),
    }
}

#[test]
fn the_windows_service_runs_up_as_the_user_at_boot_and_restarts_on_failure() {
    let expected = r#"service: adk-rust-kernel-0123456789abcdef
display name: ADK-Rust agent kernel
description: Runs `adk-rust up` for the kernel home C:\Users\ana\AppData\Roaming\adk-rust
command line: "C:\Program Files\adk-rust\adk-rust.exe" up --home C:\Users\ana\AppData\Roaming\adk-rust --dir "C:\Users\ana\my project\\" --windows-service adk-rust-kernel-0123456789abcdef
account: DESKTOP-1\ana
start: automatic (delayed), at boot
on failure: restart after 5 s, 10 s, 30 s; the count resets after a day; an exit with an error counts as a failure
working directory: C:\Users\ana\my project
environment: PATH=C:\Users\ana\.cargo\bin;C:\Windows\system32
log: C:\Users\ana\AppData\Roaming\adk-rust\kernel.log
"#;
    assert_eq!(windows_definition().render(ServicePlatform::Windows), expected);
}

#[test]
fn windows_words_are_quoted_as_the_c_runtime_parses_them() {
    use super::windows::quote;

    let cases = [
        ("--home", "--home"),
        ("", r#""""#),
        ("a b", r#""a b""#),
        (r#"say "hi""#, r#""say \"hi\"""#),
        (r"C:\dir with space\", r#""C:\dir with space\\""#),
        (r#"a\"b c"#, r#""a\\\"b c""#),
        (r"C:\no\space\", r"C:\no\space\"),
    ];
    for (word, quoted) in cases {
        assert_eq!(quote(word), quoted, "{word}");
    }
}

#[test]
fn each_kernel_home_has_its_own_service_name() {
    let default = ServiceDefinition::name_for(Path::new("/home/ana/.config/adk-rust"));
    assert!(default.starts_with("adk-rust-kernel-"), "{default}");
    assert_eq!(default.len(), "adk-rust-kernel-".len() + 16);
    assert_eq!(default, ServiceDefinition::name_for(Path::new("/home/ana/.config/adk-rust")));
    assert_ne!(default, ServiceDefinition::name_for(Path::new("/srv/kernel")));

    let definition = ServiceDefinition::new("/usr/bin/adk-rust", "/srv/kernel");
    assert_eq!(definition.name, ServiceDefinition::name_for(Path::new("/srv/kernel")));
    assert_eq!(definition.args, strings(&["up", "--home", "/srv/kernel"]));
    assert_eq!(definition.working_directory, Path::new("/srv/kernel"));
}

#[test]
fn the_current_platform_has_its_service_form() {
    let expected = if cfg!(target_os = "macos") {
        Some(ServicePlatform::Launchd)
    } else if cfg!(target_os = "linux") {
        Some(ServicePlatform::Systemd)
    } else if cfg!(windows) {
        Some(ServicePlatform::Windows)
    } else {
        None
    };
    assert_eq!(ServicePlatform::current(), expected);
}

/// launchd's own parser accepts the plist.
#[cfg(target_os = "macos")]
#[test]
fn plutil_accepts_the_agent_plist() {
    let dir = tempfile::tempdir().unwrap();
    let plist = dir.path().join("agent.plist");
    let definition = unix_definition("/tmp/a&b").with_args(["up", "--instruction", "<x>"]);
    std::fs::write(&plist, definition.render(ServicePlatform::Launchd)).unwrap();
    let output = std::process::Command::new("plutil").arg("-lint").arg(&plist).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
}

#[cfg(unix)]
#[test]
fn the_log_is_created_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let definition = ServiceDefinition::new("/usr/bin/adk-rust", &home);
    let log = super::prepare_log(&definition).unwrap();
    assert_eq!(log, home.join(super::LOG_FILE));
    let mode = std::fs::metadata(&log).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    // An existing log is appended to, never truncated.
    std::fs::write(&log, "earlier\n").unwrap();
    super::prepare_log(&definition).unwrap();
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "earlier\n");
}
