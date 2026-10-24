//! Spec 3.3 §5.2 scenario: the terminal on a kernel-hosted harness. A scripted turn holds a
//! write; `--output json` ends waiting for the decision with exit code 3; the full screen,
//! parsed as a terminal would, draws the decision box from the inbox entry in the §9 dark
//! tokens; the answer the `y` key sends lets the parked process run the write once; the next
//! JSON run ends done.
//!
//! The model is `ScriptedLlm`; the kernel, its durable worker, the harness, the approval
//! service and the session store are real.

#![cfg(all(feature = "tui", feature = "web"))]

mod common;

use std::time::Duration;

use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_surface::tui::{EXIT_DONE, EXIT_WAITING, TerminalSurface, ThemeChoice};
use adk_surface::{Answer, Caller, Command, Origin, SurfaceKind};
use common::*;
use serde_json::{Value, json};
use std::sync::Arc;

const WITHIN: Duration = Duration::from_secs(15);

fn terminal() -> Caller {
    Caller::human(OWNER, SurfaceKind::Terminal, Origin::Keyboard)
        .with_groups(["owner"])
        .with_paired_local()
}

fn json_lines(out: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(out)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|_| panic!("not JSON: {line:?}")))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_terminal_waits_on_a_held_call_draws_it_and_answers_it() {
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "invoice.txt", "content": "INV-1" }),
            ),
            ScriptedResponse::text("The invoice is written."),
        ],
    ));
    let s = scenario(model).await;
    let surface = || {
        TerminalSurface::new(s.engine.clone(), terminal())
            .session("s1")
            .poll_interval(Duration::from_millis(20))
    };

    // JSON lines: the message is sent, the turn holds its write, and the run ends waiting.
    let mut out = Vec::new();
    let code =
        surface().run_json(Some("Write the October invoice".into()), &mut out).await.unwrap();
    let lines = json_lines(&out);
    assert_eq!(code, EXIT_WAITING, "{lines:?}");
    assert_eq!(lines[0]["type"], "command");
    let end = lines.last().unwrap();
    assert_eq!(
        (end["type"].clone(), end["outcome"].clone(), end["exitCode"].clone()),
        (json!("end"), json!("waitingForYou"), json!(3))
    );
    assert!(!s.path.join("invoice.txt").exists(), "held, not written");

    // One full-screen frame, as a terminal receives it.
    let bytes = surface().theme(ThemeChoice::Dark).screen(150, 40).await.unwrap();
    let mut parser = vt100::Parser::new(40, 150, 0);
    parser.process(&bytes);
    let shown = parser.screen().contents();
    for expected in [
        "Waiting for you · Write file invoice.txt",
        "Medium risk · Can't be undone",
        "path     invoice.txt",
        "Allow once",
        "Edit",
        "Deny",
        "Details",
        "Decisions  1",
        "esc never decides",
    ] {
        assert!(shown.contains(expected), "{expected:?} missing:\n{shown}");
    }
    // The §9 dark background, #0A1628, in 24-bit colour.
    let navy = b"48;2;10;22;40m";
    assert!(bytes.windows(navy.len()).any(|window| window == navy));

    // The answer `y` sends: allow once, at the version on screen.
    let card = s.engine.decisions(&terminal()).await.unwrap().remove(0);
    let decide = Command::Decide {
        decision_id: card.id,
        version: card.version,
        answer: Answer::AllowOnce,
        edited_args: None,
        reason: None,
        expanded: true,
    };
    s.engine.execute(&terminal(), decide).await.unwrap();
    eventually("the allowed write to run", WITHIN, || async {
        s.path.join("invoice.txt").exists().then_some(())
    })
    .await;

    // Nothing waits any more: the next JSON run ends done once the turn settles.
    let mut out = Vec::new();
    let code = surface().run_json(None, &mut out).await.unwrap();
    let lines = json_lines(&out);
    assert_eq!(code, EXIT_DONE, "{lines:?}");
    assert_eq!(std::fs::read_to_string(s.path.join("invoice.txt")).unwrap(), "INV-1");
}
