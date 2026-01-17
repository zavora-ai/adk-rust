// tests/cli_tests.rs

use assert_cmd::Command;

#[test]
fn add_command_works() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::cargo_bin("calculator_cli")?;
    cmd.arg("add").arg("5").arg("3");
    cmd.assert().stdout(predicates::str::contains("Add: 5 + 3 = ?"));
    Ok(())
}

#[test]
fn subtract_command_works() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::cargo_bin("calculator_cli")?;
    cmd.arg("subtract").arg("10").arg("4");
    cmd.assert().stdout(predicates::str::contains("Subtract: 10 - 4 = ?"));
    Ok(())
}

#[test]
fn multiply_command_works() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::cargo_bin("calculator_cli")?;
    cmd.arg("multiply").arg("6").arg("7");
    cmd.assert().stdout(predicates::str::contains("Multiply: 6 * 7 = ?"));
    Ok(())
}

#[test]
fn divide_command_works() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::cargo_bin("calculator_cli")?;
    cmd.arg("divide").arg("10").arg("2");
    cmd.assert().stdout(predicates::str::contains("Divide: 10 / 2 = ?"));
    Ok(())
}

#[test]
fn no_args_shows_help() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::cargo_bin("calculator_cli")?;
    cmd.assert().stderr(predicates::str::contains("Usage: calculator_cli <COMMAND>"));
    Ok(())
}

#[test]
fn unknown_command_shows_error() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::cargo_bin("calculator_cli")?;
    cmd.arg("unknown");
    cmd.assert().stderr(predicates::str::contains("error: unrecognized subcommand 'unknown'"));
    Ok(())
}
