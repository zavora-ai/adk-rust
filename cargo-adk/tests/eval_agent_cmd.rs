//! `cargo adk eval --agent-cmd` runs a real agent process over the JSONL protocol and
//! exits non-zero when a case fails, the agent errors, or a score regresses.
//!
//! The fake agents are shell and Python scripts, so these tests run on Unix only.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Output;

use tempfile::TempDir;

/// Answers the weather question correctly, fails on `boom`, and knows nothing else.
const WEATHER_AGENT: &str = r#"
while IFS= read -r line; do
  case "$line" in
    *'"user_text":"Weather in Nairobi?"'*)
      printf '%s\n' '{"text":"Sunny in Nairobi.","tool_calls":[{"name":"get_weather","args":{"city":"Nairobi"}}]}' ;;
    *'"user_text":"boom"'*)
      printf '%s\n' '{"error":"model quota exhausted"}' ;;
    *)
      printf '%s\n' '{"text":"I do not know."}' ;;
  esac
done
"#;

/// Answers without calling the weather tool and with different text.
const DEGRADED_AGENT: &str = r#"
while IFS= read -r line; do
  printf '%s\n' '{"text":"Probably cloudy somewhere."}'
done
"#;

/// Remembers a name per session and reports the turn index it was given.
const MEMORY_AGENT: &str = r#"
import json, sys
names = {}
for line in sys.stdin:
    request = json.loads(line)
    session, text = request["session_id"], request["user_text"]
    if text.startswith("My name is "):
        names[session] = text[len("My name is "):].rstrip(".")
        reply = "Hello."
    else:
        reply = f"You are {names.get(session, 'a stranger')} (turn {request['turn']}, case {request['case_id']})."
    print(json.dumps({"text": reply}), flush=True)
"#;

const WEATHER_SET: &str = r#"{
    "eval_set_id": "weather",
    "name": "Weather",
    "eval_cases": [{
        "eval_id": "current_weather",
        "conversation": [{
            "invocation_id": "inv_1",
            "user_content": {"parts": [{"text": "Weather in Nairobi?"}]},
            "final_response": {"parts": [{"text": "Sunny in Nairobi."}], "role": "model"},
            "intermediate_data": {"tool_uses": [{"name": "get_weather", "args": {"city": "Nairobi"}}]}
        }]
    }]
}"#;

fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

fn cargo_adk_eval(args: &[&str]) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_cargo-adk"))
        .args(["adk", "eval"])
        .args(args)
        .output()
        .expect("cargo-adk runs")
}

fn text(output: &Output) -> String {
    format!(
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn agent_cmd(dir: &Path, name: &str, script: &str, interpreter: &str) -> String {
    let script = write(dir, name, script);
    format!("{interpreter} {}", script.display())
}

#[test]
fn a_passing_agent_exits_zero() {
    let dir = TempDir::new().unwrap();
    let set = write(dir.path(), "weather.test.json", WEATHER_SET);
    let agent = agent_cmd(dir.path(), "agent.sh", WEATHER_AGENT, "sh");

    let output = cargo_adk_eval(&[set.to_str().unwrap(), "--agent-cmd", &agent]);

    assert!(output.status.success(), "{}", text(&output));
    assert!(text(&output).contains("Passed: 1  Failed: 0"), "{}", text(&output));
}

#[test]
fn a_wrong_answer_exits_non_zero() {
    let dir = TempDir::new().unwrap();
    let set = write(dir.path(), "weather.test.json", WEATHER_SET);
    let agent = agent_cmd(dir.path(), "agent.sh", DEGRADED_AGENT, "sh");

    let output = cargo_adk_eval(&[set.to_str().unwrap(), "--agent-cmd", &agent]);

    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    let report = text(&output);
    assert!(report.contains("FAILED current_weather"), "{report}");
    assert!(report.contains("tool_trajectory"), "{report}");
}

#[test]
fn an_agent_error_fails_the_case() {
    let dir = TempDir::new().unwrap();
    let set =
        write(dir.path(), "boom.test.json", &WEATHER_SET.replace("Weather in Nairobi?", "boom"));
    let agent = agent_cmd(dir.path(), "agent.sh", WEATHER_AGENT, "sh");

    let output = cargo_adk_eval(&[set.to_str().unwrap(), "--agent-cmd", &agent]);

    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("model quota exhausted"), "{}", text(&output));
}

#[test]
fn protocol_violations_fail_closed() {
    let dir = TempDir::new().unwrap();
    let set = write(dir.path(), "weather.test.json", WEATHER_SET);

    for (agent, expected) in [
        ("true", "closed stdout before answering"),
        ("echo starting up; cat >/dev/null", "not a valid response line"),
    ] {
        let output = cargo_adk_eval(&[set.to_str().unwrap(), "--agent-cmd", agent]);
        assert_eq!(output.status.code(), Some(1), "{agent}: {}", text(&output));
        assert!(text(&output).contains(expected), "{agent}: {}", text(&output));
    }

    let output =
        cargo_adk_eval(&[set.to_str().unwrap(), "--agent-cmd", "sleep 5", "--turn-timeout", "1"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("did not answer within 1s"), "{}", text(&output));
}

#[test]
fn turns_of_a_case_share_a_session_and_cases_do_not() {
    let dir = TempDir::new().unwrap();
    let set = write(
        dir.path(),
        "memory.test.json",
        r#"{
            "eval_set_id": "memory",
            "name": "Memory",
            "eval_cases": [
                {
                    "eval_id": "ada",
                    "conversation": [
                        {"invocation_id": "1", "user_content": {"parts": [{"text": "My name is Ada."}]},
                         "final_response": {"parts": [{"text": "Hello."}]}},
                        {"invocation_id": "2", "user_content": {"parts": [{"text": "Who am I?"}]},
                         "final_response": {"parts": [{"text": "You are Ada (turn 1, case ada)."}]}}
                    ]
                },
                {
                    "eval_id": "stranger",
                    "conversation": [
                        {"invocation_id": "1", "user_content": {"parts": [{"text": "Who am I?"}]},
                         "final_response": {"parts": [{"text": "You are a stranger (turn 0, case stranger)."}]}}
                    ]
                }
            ]
        }"#,
    );
    let criteria = write(
        dir.path(),
        "criteria.json",
        r#"{"response_similarity": 1.0, "response_match_config": {"algorithm": "exact"}}"#,
    );
    let agent = agent_cmd(dir.path(), "agent.py", MEMORY_AGENT, "python3");

    let output = cargo_adk_eval(&[
        set.to_str().unwrap(),
        "--agent-cmd",
        &agent,
        "--criteria",
        criteria.to_str().unwrap(),
    ]);

    assert!(output.status.success(), "{}", text(&output));
    assert!(text(&output).contains("Passed: 2  Failed: 0"), "{}", text(&output));
}

#[test]
fn regression_checks_need_a_baseline_and_catch_drops() {
    let dir = TempDir::new().unwrap();
    let set = write(dir.path(), "weather.test.json", WEATHER_SET);
    let good = agent_cmd(dir.path(), "good.sh", WEATHER_AGENT, "sh");
    let degraded = agent_cmd(dir.path(), "degraded.sh", DEGRADED_AGENT, "sh");
    let baseline = dir.path().join(".eval-baseline.json");
    let set_arg = set.to_str().unwrap();

    // A missing baseline is refused before the agent runs.
    let output = cargo_adk_eval(&[set_arg, "--agent-cmd", "exit 3", "--check-regression"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("needs a baseline, and there is none"), "{}", text(&output));

    let output = cargo_adk_eval(&[set_arg, "--agent-cmd", &good, "--save-baseline"]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(baseline.exists());

    let output = cargo_adk_eval(&[set_arg, "--agent-cmd", &good, "--check-regression"]);
    assert!(output.status.success(), "{}", text(&output));

    // Accept the failing cases so only the regression check decides the exit code.
    let criteria = write(dir.path(), "lenient.json", r#"{"response_similarity": 0.0}"#);
    let output = cargo_adk_eval(&[
        set_arg,
        "--agent-cmd",
        &degraded,
        "--criteria",
        criteria.to_str().unwrap(),
        "--check-regression",
    ]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains("regression: response_similarity [current_weather]"),
        "{}",
        text(&output)
    );
}

#[test]
fn a_baseline_is_not_saved_from_a_run_that_errored() {
    let dir = TempDir::new().unwrap();
    let set = write(dir.path(), "weather.test.json", WEATHER_SET);

    let output = cargo_adk_eval(&[set.to_str().unwrap(), "--agent-cmd", "true", "--save-baseline"]);

    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("not saving a baseline"), "{}", text(&output));
    assert!(!dir.path().join(".eval-baseline.json").exists());
}

#[test]
fn junit_output_is_written_to_the_requested_file() {
    let dir = TempDir::new().unwrap();
    let set = write(dir.path(), "weather.test.json", WEATHER_SET);
    let agent = agent_cmd(dir.path(), "agent.sh", WEATHER_AGENT, "sh");
    let xml = dir.path().join("results.xml");

    let output = cargo_adk_eval(&[
        set.to_str().unwrap(),
        "--agent-cmd",
        &agent,
        "--format",
        "junit",
        "--output",
        xml.to_str().unwrap(),
    ]);

    assert!(output.status.success(), "{}", text(&output));
    let xml = std::fs::read_to_string(xml).unwrap();
    assert!(xml.contains(r#"<testsuite name="weather" tests="1" failures="0""#), "{xml}");
}

#[test]
fn judged_criteria_without_a_judge_are_refused_before_running() {
    let dir = TempDir::new().unwrap();
    let set = write(dir.path(), "weather.test.json", WEATHER_SET);
    let criteria = write(dir.path(), "criteria.json", r#"{"safety_score": 0.9}"#);

    let output = cargo_adk_eval(&[
        set.to_str().unwrap(),
        "--agent-cmd",
        "exit 3",
        "--criteria",
        criteria.to_str().unwrap(),
    ]);

    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("need an LLM judge"), "{}", text(&output));
}
