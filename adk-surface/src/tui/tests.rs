//! The terminal's keys and rendering (spec 3.3 §5.2, §7.4, §11 S-I4, S-I9).

use std::time::{Duration, Instant};

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use proptest::prelude::*;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::{Value, json};

use super::app::{ARM_DELAY, App, Effect, Input, Update, View};
use super::inline::Printer;
use super::markdown::wrap_spans;
use super::theme::{Appearance, Theme};
use super::view::Renderer;
use crate::engine::{Command, FeedFrame};
use crate::model::{
    AgentRef, AgentRow, AgentStatus, Avatar, CallView, CardStatus, DecisionCard, FeedEntry,
    Reversibility, Speaker, ThreadItem, TimelineStep,
};
use crate::rules::{Allowed, Answer, AnswerOption, Risk};

const SESSION: &str = "s1";

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent { modifiers: KeyModifiers::CONTROL, ..key(KeyCode::Char(c)) }
}

fn option(answer: Answer, label: &str, needs_expanded: bool) -> AnswerOption {
    AnswerOption { answer, label: label.into(), needs_presence: false, needs_expanded }
}

fn agent(name: &str) -> AgentRef {
    AgentRef {
        name: name.to_lowercase(),
        display_name: name.into(),
        avatar: Avatar { monogram: name.chars().take(1).collect(), color: "#C03A00".into() },
    }
}

fn card(id: &str, tool: &str, args: Value) -> DecisionCard {
    let truncated = args.to_string().len() > crate::CALL_TEXT_LIMIT;
    DecisionCard {
        id: id.into(),
        version: 1,
        session_id: SESSION.into(),
        agent: agent("Finance"),
        waiting_since: Utc::now(),
        since_text: "since 18:00".into(),
        status: CardStatus::Waiting,
        status_text: "Waiting for you".into(),
        heading: format!("Send email to {}", args["to"].as_str().unwrap_or("[client email]")),
        call: CallView { tool: tool.into(), text: format!("{tool}({args})"), args, truncated },
        timeline: vec![
            TimelineStep { text: "Asked at 18:00".into(), current: false },
            TimelineStep { text: "Waited through quiet time".into(), current: false },
            TimelineStep { text: "Waiting for you".into(), current: true },
        ],
        allowed: Allowed {
            options: vec![
                option(Answer::AllowOnce, "Allow once", truncated),
                option(Answer::AlwaysAllow, "Always allow for this person", truncated),
                option(Answer::Edit, "Edit", truncated),
                option(Answer::Deny, "Deny", false),
            ],
            open_in_home: false,
            cancel_window_secs: None,
            note: None,
        },
        risk: Risk::Medium,
        risk_text: "Medium risk".into(),
        reversibility: Reversibility::CantBeUnsent,
        reversibility_text: "Can't be unsent".into(),
        money: false,
        also_shown: Some("Also on your phone and in Telegram".into()),
        decided: None,
    }
}

fn invoice() -> DecisionCard {
    card(
        "apr_1",
        "send_email",
        json!({ "to": "[client email]", "subject": "Invoice INV-[NUMBER] for October" }),
    )
}

/// An app showing `cards`, which appeared at `shown`.
fn app_with(cards: Vec<DecisionCard>, shown: Instant) -> App {
    let mut app = App::new("adk-rust", SESSION.into());
    app.update(
        Update::Frame { session: SESSION.into(), frame: FeedFrame::Decisions { cards } },
        shown,
    );
    app
}

fn decides(effects: &[Effect]) -> bool {
    effects.iter().any(|effect| matches!(effect, Effect::Execute(Command::Decide { .. })))
}

fn type_text(app: &mut App, text: &str, at: Instant) -> Vec<Effect> {
    let mut effects = Vec::new();
    for c in text.chars() {
        effects.extend(app.key(key(KeyCode::Char(c)), at));
    }
    effects
}

#[test]
fn a_decision_key_types_into_the_message_until_the_box_is_armed() {
    let shown = Instant::now();
    let mut app = app_with(vec![invoice()], shown);
    let effects = app.key(key(KeyCode::Char('y')), shown + Duration::from_millis(100));
    assert!(effects.is_empty());
    assert_eq!(app.composer.text, "y", "the key typed into the message box");

    let mut app = app_with(vec![invoice()], shown);
    let effects = app.key(key(KeyCode::Char('y')), shown + ARM_DELAY);
    assert_eq!(
        effects,
        vec![Effect::Execute(Command::Decide {
            decision_id: "apr_1".into(),
            version: 1,
            answer: Answer::AllowOnce,
            edited_args: None,
            reason: None,
            expanded: true,
        })]
    );
}

#[test]
fn esc_never_decides_and_switches_to_typing() {
    let shown = Instant::now();
    let armed = shown + ARM_DELAY;
    let mut app = app_with(vec![invoice()], shown);
    for _ in 0..5 {
        assert!(!decides(&app.key(key(KeyCode::Esc), armed)));
    }
    assert!(app.typing, "an odd number of Esc presses leaves typing on");
    let effects = type_text(&mut app, "yes", armed);
    assert!(!decides(&effects));
    assert_eq!(app.composer.text, "yes");
    let effects = app.key(key(KeyCode::Enter), armed);
    assert_eq!(
        effects,
        vec![Effect::Execute(Command::Send { session_id: SESSION.into(), text: "yes".into() })],
        "typed while a decision is open: sent, and the engine queues it (S-I4)"
    );
}

#[test]
fn a_keystroke_into_the_message_disarms_the_keys_for_a_moment() {
    let shown = Instant::now();
    let armed = shown + ARM_DELAY;
    let mut app = app_with(vec![invoice()], shown);
    type_text(&mut app, "x", armed);
    app.key(key(KeyCode::Backspace), armed);
    assert!(app.composer.is_empty());
    let effects = app.key(key(KeyCode::Char('n')), armed + Duration::from_millis(50));
    assert!(effects.is_empty() && app.input == Input::Message, "typed, not denied");
    assert_eq!(app.composer.text, "n");
}

#[test]
fn n_asks_for_a_reason_then_denies_and_e_edits_as_json() {
    let shown = Instant::now();
    let armed = shown + ARM_DELAY;
    let mut app = app_with(vec![invoice()], shown);
    assert!(app.key(key(KeyCode::Char('n')), armed).is_empty());
    assert_eq!(app.input, Input::DenyReason { id: "apr_1".into() });
    // Esc cancels the reason: nothing was decided.
    assert!(!decides(&app.key(key(KeyCode::Esc), armed)));
    assert_eq!(app.input, Input::Message);
    app.key(key(KeyCode::Char('n')), armed + ARM_DELAY * 2);
    type_text(&mut app, "not today", armed + ARM_DELAY * 2);
    let effects = app.key(key(KeyCode::Enter), armed + ARM_DELAY * 2);
    assert!(matches!(
        &effects[..],
        [Effect::Execute(Command::Decide { answer: Answer::Deny, reason: Some(reason), .. })] if reason == "not today"
    ));

    let mut app = app_with(vec![invoice()], shown);
    app.key(key(KeyCode::Char('e')), armed);
    assert_eq!(app.input, Input::EditArgs { id: "apr_1".into() });
    let edited: Value = serde_json::from_str(&app.composer.text).unwrap();
    assert_eq!(edited["to"], "[client email]");
    app.key(ctrl('u'), armed);
    type_text(&mut app, "not json", armed);
    assert!(app.key(key(KeyCode::Enter), armed).is_empty(), "invalid JSON is not sent");
    assert!(app.notices.last().unwrap().text.contains("not JSON"));
    app.key(ctrl('u'), armed);
    type_text(&mut app, r#"{"to":"[accountant]"}"#, armed);
    let effects = app.key(key(KeyCode::Enter), armed);
    assert!(matches!(
        &effects[..],
        [Effect::Execute(Command::Decide { answer: Answer::Edit, edited_args: Some(args), .. })] if args["to"] == "[accountant]"
    ));
}

#[test]
fn a_long_call_is_answered_only_after_its_details() {
    let shown = Instant::now();
    let armed = shown + ARM_DELAY;
    let long = card("apr_2", "write_file", json!({ "path": "a", "content": "x".repeat(3_000) }));
    assert!(long.call.truncated);
    let mut app = app_with(vec![long], shown);
    assert!(app.key(key(KeyCode::Char('y')), armed).is_empty());
    assert!(app.notices.last().unwrap().text.contains("Press ?"));
    app.key(key(KeyCode::Char('?')), armed);
    let effects = app.key(key(KeyCode::Char('y')), armed);
    assert!(matches!(
        &effects[..],
        [Effect::Execute(Command::Decide { answer: Answer::AllowOnce, expanded: true, .. })]
    ));
}

#[test]
fn an_answer_the_terminal_may_not_give_says_why() {
    let shown = Instant::now();
    let mut high = invoice();
    high.allowed.options.retain(|option| option.answer == Answer::Deny);
    high.allowed.note = Some(crate::words::NEEDS_A_SCREEN.into());
    let mut app = app_with(vec![high], shown);
    assert!(app.key(key(KeyCode::Char('y')), shown + ARM_DELAY).is_empty());
    assert_eq!(app.notices.last().unwrap().text, crate::words::NEEDS_A_SCREEN);
}

#[test]
fn a_changed_decision_arms_again() {
    let shown = Instant::now();
    let mut app = app_with(vec![invoice()], shown);
    let later = shown + ARM_DELAY * 3;
    let mut changed = invoice();
    changed.version = 2;
    app.update(
        Update::Frame {
            session: SESSION.into(),
            frame: FeedFrame::Decisions { cards: vec![changed] },
        },
        later,
    );
    assert!(!app.keys_answer(later + Duration::from_millis(100)));
    assert!(app.keys_answer(later + ARM_DELAY));
}

#[test]
fn stop_all_and_the_mode_from_the_keyboard() {
    let now = Instant::now();
    let mut app = App::new("adk-rust", SESSION.into());
    assert_eq!(app.key(ctrl('k'), now), vec![Effect::Execute(Command::StopAll { reason: None })]);
    app.key(ctrl('r'), now);
    assert_eq!(app.view, View::Permissions);
    assert_eq!(
        app.key(key(KeyCode::Char('3')), now),
        vec![Effect::Execute(Command::SetMode {
            session_id: SESSION.into(),
            mode: crate::Mode::AskFirst
        })]
    );
    assert_eq!(app.key(key(KeyCode::Char('r')), now), vec![Effect::Execute(Command::ResumeAll)]);
    app.key(key(KeyCode::Esc), now);
    assert_eq!(app.view, View::Thread);
    type_text(&mut app, "/mode go ahead", now);
    assert_eq!(
        app.key(key(KeyCode::Enter), now),
        vec![Effect::Execute(Command::SetMode {
            session_id: SESSION.into(),
            mode: crate::Mode::GoAhead
        })]
    );
}

fn screen_text(app: &App, appearance: Appearance, cols: u16, rows: u16) -> Vec<String> {
    let mut renderer = Renderer::new(Theme::new(appearance, true));
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
    let now = Instant::now() + ARM_DELAY * 4;
    terminal.draw(|frame| renderer.draw(frame, app, now)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..rows)
        .map(|y| (0..cols).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>())
        .collect()
}

fn thread_entry(seq: u64, items: Vec<ThreadItem>) -> FeedEntry {
    FeedEntry { seq, at: Utc::now(), at_text: "17:58".into(), items }
}

#[test]
fn the_full_screen_has_the_layout_of_the_concept() {
    let shown = Instant::now();
    let mut app = app_with(vec![invoice()], shown);
    app.update(
        Update::Agents(vec![AgentRow {
            agent: agent("Finance"),
            status: AgentStatus::WaitingForYou,
            status_text: "Waiting for you".into(),
            wake_at: None,
            session_id: Some(SESSION.into()),
        }]),
        shown,
    );
    let entry = thread_entry(
        1,
        vec![ThreadItem::Message {
            from: Speaker::Agent,
            author: "Finance".into(),
            text: "The October invoice for [CLIENT] is ready.".into(),
        }],
    );
    app.update(Update::Frame { session: SESSION.into(), frame: FeedFrame::Entry { entry } }, shown);
    app.queued = vec!["also cc my accountant".into()];
    let lines = screen_text(&app, Appearance::Light, 150, 40);
    let all = lines.join("\n");
    assert!(lines[0].contains("adk-rust") && lines[0].contains("Decisions"), "{}", lines[0]);
    for expected in [
        "Agents",
        "◆ Finance",
        "Coming up",
        "While you were away",
        "Finance · 17:58",
        "The October invoice for [CLIENT] is ready.",
        "Waiting for you · Send email to [client email]",
        "Medium risk · Can't be unsent",
        "send_email",
        "subject  Invoice INV-[NUMBER] for October",
        "Asked at 18:00 · Waited through quiet time · Waiting for you",
        " y  Allow once",
        " a  Always allow for this person",
        " e  Edit",
        " n  Deny",
        " ?  Details",
        "Also on your phone and in Telegram.",
        "Queued › also cc my accountant",
        "esc never decides",
    ] {
        assert!(all.contains(expected), "{expected:?} missing:\n{all}");
    }
    assert!(lines[39].contains("ctrl-k stop all"), "{}", lines[39]);
}

/// `--output json` on a source with no turn in it: how the run ends, as the exit code and
/// the last line.
async fn json_end(daily_limit: Option<u64>) -> (i32, Value) {
    use std::sync::Arc;

    use adk_core::{ApprovalService, InMemoryApprovalStore, StoreApprovalService};
    use adk_session::{InMemorySessionService, SessionService};

    use crate::{Caller, LocalSource, Origin, SurfaceEngine, SurfaceKind};

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let approvals: Arc<dyn ApprovalService> =
        Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())));
    let mut source = LocalSource::builder("adk-rust").sessions(sessions).approvals(approvals);
    if let Some(limit) = daily_limit {
        source = source.daily_limit(limit);
    }
    let engine = SurfaceEngine::builder(Arc::new(source.build().unwrap())).build();
    let me = Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard);
    let mut out = Vec::new();
    let code = super::TerminalSurface::new(engine, me)
        .session(SESSION)
        .poll_interval(Duration::from_millis(20))
        .run_json(None, &mut out)
        .await
        .unwrap();
    let text = String::from_utf8(out).unwrap();
    let end = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    (code, end)
}

#[tokio::test]
async fn json_exit_codes_say_done_and_the_daily_spending_limit() {
    let (code, end) = json_end(None).await;
    assert_eq!(code, super::EXIT_DONE);
    assert_eq!(end["outcome"], "done");

    // Today's spend has reached the limit: CI sees 4, not a failure.
    let (code, end) = json_end(Some(0)).await;
    assert_eq!(code, super::EXIT_BUDGET);
    assert_eq!(end["type"], "end");
    assert_eq!(end["outcome"], "budgetReached");
    assert_eq!(end["exitCode"], 4);
    assert_eq!(end["sessionId"], SESSION);
}

/// Characters a model, a tool or a file may send to take over a terminal.
fn hostile() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            Just('\x1b'),
            Just('\x07'),
            Just('\r'),
            Just('\x08'),
            Just('\0'),
            Just('\x7f'),
            Just('\u{9b}'),
            Just('\u{85}'),
            Just('\u{202e}'),
            Just('\u{2066}'),
            Just('\u{200b}'),
            Just('\u{feff}'),
            Just('\n'),
            Just('\t'),
            Just(' '),
            Just('['),
            Just(']'),
            Just(';'),
            Just('`'),
            Just('*'),
            prop::char::range('a', 'z'),
            prop::char::range('0', '9'),
        ],
        0..60,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

fn is_hostile(c: char) -> bool {
    (c.is_control() && c != '\n')
        || matches!(c, '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Whatever a model, a tool or a file wrote, the full screen, the inline output and the
    /// JSON lines carry no control or reordering character of theirs (spec 3.3 §11 S-I9).
    #[test]
    fn rendered_output_never_carries_control_characters(
        message in hostile(),
        tool in hostile(),
        arg_key in hostile(),
        arg_value in hostile(),
        heading in hostile(),
        name in hostile(),
        queued in hostile(),
    ) {
        let shown = Instant::now();
        let mut decision = card("apr_9", &tool, json!({ arg_key.clone(): arg_value.clone(), "to": arg_value }));
        decision.heading = heading;
        decision.agent.display_name = name.clone();
        let mut app = app_with(vec![decision], shown);
        app.update(Update::Agents(vec![AgentRow {
            agent: AgentRef { display_name: name.clone(), ..agent("x") },
            status: AgentStatus::Working,
            status_text: "Working".into(),
            wake_at: None,
            session_id: Some(SESSION.into()),
        }]), shown);
        let entry = thread_entry(1, vec![
            ThreadItem::Message { from: Speaker::Agent, author: name.clone(), text: message.clone() },
            ThreadItem::Message { from: Speaker::You, author: "You".into(), text: message.clone() },
            ThreadItem::Activity { tool: tool.clone(), text: message.clone(), call_id: Some("c1".into()) },
            ThreadItem::Result { tool: tool.clone(), call_id: None, ok: false, text: "Failed".into() },
            ThreadItem::Decision { decision_id: None, tool: tool.clone(), text: "Waiting for you".into() },
        ]);
        app.update(Update::Frame { session: SESSION.into(), frame: FeedFrame::Entry { entry: entry.clone() } }, shown);
        app.update(Update::Error(message.clone()), shown);
        app.queued = vec![queued];

        // Full screen: every cell.
        for appearance in [Appearance::Light, Appearance::Dark] {
            let lines = screen_text(&app, appearance, 100, 60);
            for line in &lines {
                prop_assert!(!line.chars().any(is_hostile), "a cell carries a control character: {line:?}");
            }
        }

        // Inline: every byte, without colour.
        let mut printer = Printer::new(Vec::new(), Theme::new(Appearance::Light, true), false, false).with_width(90);
        printer.sync(&app, shown + ARM_DELAY).unwrap();
        let printed = String::from_utf8(printer.into_inner()).unwrap();
        prop_assert!(!printed.chars().any(is_hostile), "inline output carries a control character: {printed:?}");

        // JSON lines: escaped by the encoder, the value unchanged.
        let frame = FeedFrame::Entry { entry };
        let line = super::json::encode(&frame).unwrap();
        prop_assert!(!line.chars().any(is_hostile), "a JSON line carries a control character: {line:?}");
        prop_assert_eq!(serde_json::from_str::<Value>(&line).unwrap(), serde_json::to_value(&frame).unwrap());
    }

    /// Once the person has typed into the message box, no key answers the decision (§7.4
    /// rule 2, S-I4): the text goes to the agent as a message, which the engine queues.
    #[test]
    fn keys_typed_into_a_message_never_decide(
        text in prop::collection::vec(prop_oneof![Just('y'), Just('a'), Just('e'), Just('n'), Just('?'), Just(' '), prop::char::range('a', 'z')], 0..40),
    ) {
        let shown = Instant::now();
        let armed = shown + ARM_DELAY;
        let mut app = app_with(vec![invoice()], shown);
        let mut effects = app.key(key(KeyCode::Char('x')), armed);
        for c in &text {
            effects.extend(app.key(key(KeyCode::Char(*c)), armed + ARM_DELAY * 2));
        }
        prop_assert!(!decides(&effects));
        prop_assert_eq!(app.input.clone(), Input::Message);
        let sent = app.key(key(KeyCode::Enter), armed + ARM_DELAY * 2);
        prop_assert!(!decides(&sent));
    }

    /// Wrapped text never runs past the pane.
    #[test]
    fn wrapped_lines_fit_their_width(text in "[a-z ]{0,200}", width in 8usize..80, indent in 0usize..10) {
        let spans = vec![ratatui::text::Span::raw(text)];
        for line in wrap_spans(spans, width, indent) {
            prop_assert!(line.width() <= width, "{line:?} is wider than {width}");
        }
    }
}
