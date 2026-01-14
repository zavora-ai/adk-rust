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
}