#![allow(unused_imports)]

#[cfg(test)]
mod tests {
    use std::process::Command;

    #[test]
    fn test_hello_world_output() {
        let output = Command::new("cargo")
            .arg("run")
            .arg("--quiet")
            .current_dir("hello-world-cli")
            .output()
            .expect("Failed to execute command");

        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "Hello, world!");
    }

    #[test]
    fn test_help_output() {
        let output = Command::new("cargo")
            .arg("run")
            .arg("--quiet")
            .arg("--")
            .arg("--help")
            .current_dir("hello-world-cli")
            .output()
            .expect("Failed to execute command");

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("A minimalist command-line interface (CLI) application developed in Rust."));
        assert!(stdout.contains("--help"));
        assert!(stdout.contains("--version"));
    }

    #[test]
    fn test_version_output() {
        let output = Command::new("cargo")
            .arg("run")
            .arg("--quiet")
            .arg("--")
            .arg("--version")
            .current_dir("hello-world-cli")
            .output()
            .expect("Failed to execute command");

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn test_invalid_arg_handling() {
        let output = Command::new("cargo")
            .arg("run")
            .arg("--quiet")
            .arg("--")
            .arg("--foo")
            .current_dir("hello-world-cli")
            .output()
            .expect("Failed to execute command");

        assert!(!output.status.success()); // Should exit with a non-zero status
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("unexpected argument '--foo'"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("Hello, world!")); // Should not print "Hello, world!"
    }
}