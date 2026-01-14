use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn hello_world_default_output() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::cargo_bin("hello_world_cli")?;
    cmd.assert()
        .success()
        .stdout("Hello, world!\n");
    Ok(())
}

#[test]
fn help_message() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::cargo_bin("hello_world_cli")?;
    cmd.arg("--help").assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));

    let mut cmd = Command::cargo_bin("hello_world_cli")?;
    cmd.arg("-h").assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
    Ok(())
}

#[test]
fn version_information() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::cargo_bin("hello_world_cli")?;
    cmd.arg("--version").assert()
        .success()
        .stdout(predicate::str::contains("hello_world_cli 0.1.0"));

    let mut cmd = Command::cargo_bin("hello_world_cli")?;
    cmd.arg("-V").assert()
        .success()
        .stdout(predicate::str::contains("hello_world_cli 0.1.0"));
    Ok(())
}

#[test]
fn invalid_argument_handling() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::cargo_bin("hello_world_cli")?;
    cmd.arg("--invalid-arg").assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument"))
        .code(2);
    Ok(())
}
