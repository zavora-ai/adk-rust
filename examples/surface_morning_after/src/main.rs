//! "The morning after" — the phase 2 scenario of spec 3.3 §12, "While you were away".
//!
//! Over one night two routines finish, one fails, an agent suggests a payment, the person
//! allows a call from their phone, and the kernel restarts. In the morning the phone reads
//! "While you were away" once: its counts and spending match what the kernel's stores hold,
//! exactly, across the restart, and every line links to its record.
//!
//! The kernel is the `adk-rust up` binary, a real child process; the phone is a paired device
//! that reaches it over HTTP with its device token, as the installed web home does; the email
//! is a supervised MCP driver this binary serves (`serve-mail-driver`). The briefing is read
//! from the kernel's own route, `GET /api/kernel/away` (spec 3.3 §10 K4), with the phone's
//! cursor. Every pass condition is read from stores, never from model prose; the run prints
//! what happened in plain words and exits non-zero when any pass condition fails.
//!
//! The night is compressed: the kernel runs on the system clock, so quiet time is a
//! one-minute window starting on a whole minute shortly ahead, and each step is labelled with
//! the time of the story it stands for.
//!
//! ```sh
//! cargo build -p adk-cli --bin adk-rust --features scripted-model,tui,postgres,mobile   # repo root
//! cargo run                                   # SQLite, scripted model
//! DATABASE_URL=postgres://postgres@127.0.0.1:5432/postgres cargo run -- --store postgres
//! cargo run -- --live                         # gemini-3.5-flash-lite
//! cargo run -- --provider anthropic           # claude-sonnet-5-5
//! ```

#[path = "../../surface_approve_tomorrow/src/mail.rs"]
mod mail;
#[path = "../../surface_approve_tomorrow/src/stage.rs"]
mod stage;

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::time::Duration;

use adk_core::RunState;
use adk_kernel::QuietHours;
use adk_kernel::control::ControlClient;
use adk_kernel::control::api::{AreaView, AwayCounts, AwayQuery, AwayResponse, ProcessesQuery};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};

use stage::{Model, Options, Report, Stage, until};

/// The bills routine: a suggestion to pay, which waits for the person.
const BILLS: &str = "Use propose_followup to suggest paying the electricity bill of 84.20 USD, \
                     due on Friday. Do not pay anything yourself.";
/// The report routine: an email, which waits for a decision.
const REPORT: &str = "Use send_email to send the weekly report to team@northwind.example with \
                      the subject \"Weekly report\" and the body \"All invoices are out.\"";
/// The statement routine, which fails.
const STATEMENT: &str = "Download the October bank statement into the statements folder.";

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("serve-mail-driver") {
        return match mail::serve().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        };
    }
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("error: {error:#}");
            return ExitCode::from(2);
        }
    };
    match run(&options).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("\nerror: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// The model's replies before the restart, in scripted mode, each with the price a provider
/// would report for it.
fn evening_replies() -> Vec<Value> {
    vec![
        // 23:00, the bills routine: a suggestion, which waits for the person.
        json!({ "call": { "name": "propose_followup", "args": {
                "title": "Pay the electricity bill of 84.20 USD",
                "rationale": "It is due on Friday and it is the only bill this week."
            } }, "cost": 0.0125 }),
        json!({ "text": "I suggested paying the electricity bill; nothing was paid.", "cost": 0.0031 }),
        // 01:00, the report routine: an email, which waits for a decision.
        json!({ "call": { "name": "send_email", "args": {
                "to": "team@northwind.example",
                "subject": "Weekly report",
                "body": "All invoices are out."
            } }, "cost": 0.021 }),
        json!({ "text": "The weekly report is sent.", "cost": 0.0044 }),
    ]
}

/// What the evening's scripted replies cost, in micro-USD.
fn evening_spend() -> u64 {
    evening_replies()
        .iter()
        .filter_map(|reply| reply["cost"].as_f64())
        .map(|usd| (usd * 1_000_000.0).round() as u64)
        .sum()
}

async fn run(options: &Options) -> Result<ExitCode> {
    println!("The morning after — spec 3.3 §12, phase 2 \"While you were away\"");
    println!("{}\n", options.describe());
    let stage = Stage::new(options, "morning_after").await?;
    let mut report = Report::default();
    let outcome = night(&stage, &mut report).await;
    let kept = stage.close(options.keep).await?;
    if let Some(root) = kept {
        println!("\nkept {}", root.display());
    }
    outcome?;
    Ok(report.finish("The morning after"))
}

/// One routine of the night.
struct Routine {
    name: &'static str,
    task: &'static str,
    fires: DateTime<Utc>,
    story: &'static str,
    pid: String,
}

async fn night(stage: &Stage, report: &mut Report) -> Result<()> {
    // The night, compressed: quiet time is a minute starting on the first whole minute at least
    // 40 seconds from now, so the quiet time the kernel serves (`HH:MM`) names it exactly.
    let quiet_start = whole_minute(Utc::now() + TimeDelta::seconds(40));
    let quiet_end = quiet_start + TimeDelta::seconds(60);
    stage.write_user_config(&format!(
        "[kernel]\nquiet_hours = \"{}-{}\"\n",
        stage::quiet_bound(quiet_start),
        stage::quiet_bound(quiet_end)
    ))?;
    stage.write_personal_answers(None, 5.0)?;

    let kernel = stage.start_kernel(Model::Answers(&evening_replies()), &[]).await?;
    let client = stage.client().await?;
    let home = stage.home.display().to_string();
    report.say(
        "21:00",
        Utc::now(),
        &format!(
            "adk-rust up runs as process {} ({}), quiet time {}–{} (22:00–07:00 in the story).",
            kernel.pid(),
            stage.store.label(),
            stage::clock(quiet_start),
            stage::clock(quiet_end),
        ),
    );

    // The phone pairs: a code from `adk-rust pair`, claimed over HTTP as the installed web
    // home claims it (spec 3.3 §10 K2).
    let (issued, output) = stage
        .run_cli(&["pair", "--home", &home, "--name", "Ana's phone"], Duration::from_secs(30))
        .await?;
    let code = output
        .split_whitespace()
        .find(|word| {
            word.len() >= 9
                && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && word.contains('-')
        })
        .map(str::to_string)
        .with_context(|| format!("`adk-rust pair` printed no code:\n{output}"))?;
    if !issued {
        bail!("`adk-rust pair` failed:\n{output}");
    }
    let claimed = ControlClient::http(stage.base_url())
        .claim_pairing(&code, Some("Ana's phone"))
        .await
        .context("the phone could not claim its pairing code")?;
    let token = claimed.token.clone();
    let phone = ControlClient::http(stage.base_url()).with_bearer_token(token.clone());
    let scopes: Vec<&str> = claimed.device.scopes.iter().map(|scope| scope.as_str()).collect();
    report.say(
        "21:01",
        Utc::now(),
        &format!(
            "Ana pairs her phone: `adk-rust pair` shows a code, the phone claims it and keeps a \
             device token that may {}.",
            scopes.join(", ")
        ),
    );

    // The mail driver, then three routines for the night.
    let instance = format!("morning-after-{}", std::process::id());
    let manifest = stage.root().join("mail.toml");
    std::fs::write(&manifest, mail::manifest(&std::env::current_exe()?, &stage.mail, &instance))?;
    let manifest_text = manifest.display().to_string();
    let (mounted, output) = stage
        .run_cli(
            &["drivers", "--home", &home, "mount", &manifest_text, "--yes"],
            Duration::from_secs(30),
        )
        .await?;
    if !mounted {
        bail!("mounting the mail driver failed:\n{output}");
    }
    let mut routines = vec![
        Routine {
            name: "bills",
            task: BILLS,
            fires: quiet_start + TimeDelta::seconds(3),
            story: "23:00",
            pid: String::new(),
        },
        Routine {
            name: "report",
            task: REPORT,
            fires: quiet_start + TimeDelta::seconds(9),
            story: "01:00",
            pid: String::new(),
        },
        Routine {
            name: "statement",
            task: STATEMENT,
            fires: quiet_start + TimeDelta::seconds(40),
            story: "04:00",
            pid: String::new(),
        },
    ];
    for routine in &mut routines {
        let cron = stage::daily_cron(routine.fires);
        let (woke, output) = stage
            .run_cli(
                &["wake", "--home", &home, "--cron", &cron, "--", routine.task],
                Duration::from_secs(30),
            )
            .await?;
        if !woke {
            bail!("creating the {} routine failed:\n{output}", routine.name);
        }
        routine.pid = output.split_whitespace().next().unwrap_or_default().to_string();
    }
    report.say(
        "21:02",
        Utc::now(),
        "Three routines are set for the night with `adk-rust wake --cron`: suggest a bill to pay \
         (23:00), email the weekly report (01:00), download the bank statement (04:00).",
    );
    if Utc::now() + TimeDelta::seconds(5) > quiet_start {
        bail!("setting up took too long: quiet time started first; run again");
    }

    // 22:00: quiet time. 23:00: the bills routine suggests a payment.
    stage::sleep_until(quiet_start).await;
    report.say("22:00", Utc::now(), "Quiet time starts.");
    let bills = &routines[0];
    let suggested = until(Duration::from_secs(60), || async {
        Ok(!client.agenda(&Default::default()).await?.is_empty()
            && finished(&client, &bills.pid).await?)
    })
    .await?;
    if !suggested {
        bail!("the bills routine made no suggestion:\n{}", kernel.log());
    }
    let item =
        client.agenda(&Default::default()).await?.into_iter().next().context("the suggestion")?;
    report.say(
        bills.story,
        Utc::now(),
        &format!(
            "The bills routine suggests \"{}\"; it waits for Ana, and nothing is paid.",
            item.title
        ),
    );

    // 01:00: the report routine asks to send an email; Ana allows it from her phone.
    let report_routine = &routines[1];
    let held = until(Duration::from_secs(60), || async {
        Ok(client.approvals().await?.iter().any(|item| item.tool_name.ends_with("send_email")))
    })
    .await?;
    if !held {
        bail!("the report routine never asked to send its email:\n{}", kernel.log());
    }
    // The phone reads the inbox and answers through it with its device token, the routes a
    // paired device may call (spec 3.3 §10 K2).
    let waiting_on_phone = phone.approvals().await.context("the phone cannot read the inbox")?;
    let decision = waiting_on_phone
        .iter()
        .find(|item| item.tool_name.ends_with("send_email"))
        .cloned()
        .context("the phone sees no email decision")?;
    let answered =
        phone.decide(&decision.id, decision.version, adk_core::ApprovalAction::Approve, None).await;
    report.say(
        report_routine.story,
        Utc::now(),
        "The report routine asks to send an email. Ana taps Allow once on her phone \
         (`POST /api/approvals/{id}/decide` with the device token).",
    );
    let status = match &answered {
        Ok(item) => format!("{:?} at version {}", item.state, item.version),
        Err(error) => error.to_string(),
    };
    report.note(&format!("the inbox answers: {status}"));
    let sent = until(Duration::from_secs(60), || async {
        Ok(mail::sent(&stage.mail).len() == 1 && finished(&client, &report_routine.pid).await?)
    })
    .await?;
    report.check("the phone's answer runs the call once", answered.is_ok() && sent, status);

    // 02:00: the kernel is killed and starts again, and from then on its model cannot be
    // reached: in scripted mode every reply fails, in live mode the provider refuses its key.
    stage::sleep_until(quiet_start + TimeDelta::seconds(18)).await;
    let pid = kernel.pid();
    let signal = kernel.kill_9()?;
    let kernel = stage.start_kernel(Model::Unreachable, &[]).await?;
    let client = stage.client().await?;
    if Utc::now() + TimeDelta::seconds(3) > routines[2].fires {
        bail!(
            "the night ran late: the kernel started again too close to the statement routine; run again"
        );
    }
    report.say(
        "02:00",
        Utc::now(),
        &format!(
            "`kill -9 {pid}` (signal {}), and `adk-rust up` starts again as process {}; \
             from now on its model cannot be reached.",
            signal.unwrap_or_default(),
            kernel.pid()
        ),
    );

    // 04:00: the statement routine fails.
    let statement = &routines[2];
    let failed = until(Duration::from_secs(60), || async {
        Ok(client.processes(&ProcessesQuery::default()).await?.iter().any(|process| {
            run_of(process, &statement.pid) && process.run.state == RunState::Failed
        }))
    })
    .await?;
    report.say(
        statement.story,
        Utc::now(),
        "The statement routine fails: its model call gets no answer.",
    );
    if !failed {
        bail!("the statement routine did not fail:\n{}", kernel.log());
    }

    // 06:59: the night is on the phone's page, and quiet time still holds the briefing back.
    stage::sleep_until(quiet_end - TimeDelta::seconds(4)).await;
    let early = phone.away(&AwayQuery::default()).await?;
    let windows = quiet_windows(&client.area_settings("default").await?);
    let early_at = Utc::now();
    report.say(
        "06:59",
        early_at,
        &format!(
            "The phone's page already holds {} lines, but quiet time still holds the morning \
             briefing back.",
            early.lines.len()
        ),
    );
    report.check(
        "no briefing before quiet time ends: the night is on the page, and the quiet time the home reads still holds",
        !early.lines.is_empty() && early_at < quiet_end && !briefing_due(&early, &windows, early_at),
        format!(
            "{} lines at {}; due: {}",
            early.lines.len(),
            stage::clock(early_at),
            briefing_due(&early, &windows, early_at)
        ),
    );

    // 07:00: quiet time ends; the phone reads "While you were away".
    stage::sleep_until(quiet_end).await;
    let away = phone.away(&AwayQuery::default()).await?;
    let morning = Utc::now();
    report.check(
        "the briefing is due once quiet time ends",
        briefing_due(&away, &windows, morning),
        format!("{} lines at {}", away.lines.len(), stage::clock(morning)),
    );
    report.say(
        "07:00",
        Utc::now(),
        "Quiet time ends. The phone reads \"While you were away\" (`GET /api/kernel/away`):",
    );
    for line in &away.lines {
        report.note(&format!(
            "{} {:?} {} → {}",
            line.at.format("%H:%M:%S"),
            line.kind,
            line.agent,
            line.record
        ));
    }
    report.note(&format!(
        "counts {:?}; spent {} micro-USD; quiet time respected: {}; waiting {}",
        away.counts, away.spent_micro_usd, away.quiet_time.respected, away.waiting
    ));

    // What the stores hold, read on their own.
    let processes = client.processes(&ProcessesQuery::default()).await?;
    let runs: BTreeMap<&str, Vec<RunState>> = routines
        .iter()
        .map(|routine| {
            let states = processes
                .iter()
                .filter(|process| run_of(process, &routine.pid))
                .map(|process| process.run.state)
                .filter(|state| matches!(state, RunState::Completed | RunState::Failed))
                .collect();
            (routine.name, states)
        })
        .collect();
    let finished_runs =
        runs.values().flatten().filter(|state| **state == RunState::Completed).count();
    let failed_runs = runs.values().flatten().filter(|state| **state == RunState::Failed).count();
    let suggestions = client.agenda(&Default::default()).await?.len();
    let waiting = client.approvals().await?.len();
    let area = client.area_settings("default").await?;
    report.check(
        "the briefing counts what the process table, the agenda and the inbox hold: 2 done, 1 failed, 1 suggestion, 1 decided",
        away.counts.done == finished_runs
            && finished_runs == 2
            && away.counts.failed == failed_runs
            && failed_runs == 1
            && away.counts.suggestions == suggestions
            && suggestions == 1
            && away.counts.decided == 1
            && away.waiting == waiting,
        format!("briefing {:?}, waiting {}; stores: runs {runs:?}, agenda {suggestions}, inbox {waiting}", away.counts, away.waiting),
    );
    // Scripted, the evening cost exactly what its replies say; a live model reports its own.
    let expected = match stage.model {
        stage::ModelKind::Scripted => Some(evening_spend()),
        stage::ModelKind::Live(_) => None,
    };
    report.check(
        "its spending matches the kernel's spend ledger across the restart, and is not zero",
        away.spent_micro_usd == area.spent_today
            && away.spent_micro_usd > 0
            && expected.is_none_or(|expected| away.spent_micro_usd == expected),
        format!(
            "briefing {} micro-USD, the kernel's daily total {}, the replies' price {expected:?}",
            away.spent_micro_usd, area.spent_today
        ),
    );
    report.check(
        "quiet time was respected: the suggestion waited, nothing it proposed ran",
        away.quiet_time.respected && away.quiet_time.breaches.is_empty(),
        format!("{:?}", away.quiet_time),
    );
    let mut linked = Vec::new();
    for line in &away.lines {
        let (status, body) =
            stage::get_json(&format!("{}{}", stage.base_url(), line.record), Some(&token)).await?;
        let same = body.pointer("/entry/seq").and_then(Value::as_u64) == Some(line.seq)
            && body.pointer("/line/kind") == Some(&serde_json::to_value(line.kind)?);
        if status != 200 || !same {
            linked.push(format!("{} → {status}", line.record));
        }
    }
    report.check(
        "every line links to its record",
        !away.lines.is_empty() && linked.is_empty(),
        linked.join(", "),
    );
    let seqs: Vec<u64> = away.lines.iter().map(|line| line.seq).collect();
    let mut unique = seqs.clone();
    unique.dedup();
    report.check(
        "no line appears twice across the restart",
        unique.len() == seqs.len() && seqs.windows(2).all(|pair| pair[0] < pair[1]),
        format!("{seqs:?}"),
    );

    // The phone marks it seen; the briefing does not come back, even after another restart.
    let through = away.through.context("the briefing covers no row")?;
    phone.mark_away_seen(None, through).await?;
    drop(kernel);
    let kernel = stage.start_kernel(Model::Answers(&[]), &[]).await?;
    let phone = ControlClient::http(stage.base_url()).with_bearer_token(token.clone());
    let again = phone.away(&AwayQuery::default()).await?;
    report.say(
        "07:05",
        Utc::now(),
        &format!(
            "Ana marks it seen. The kernel restarts again (process {}); the phone reads \"While you were away\" again.",
            kernel.pid()
        ),
    );
    report.check(
        "exactly one briefing: once seen, it does not come back, across a restart",
        is_quiet(&again),
        format!("{} lines, counts {:?}", again.lines.len(), again.counts),
    );
    let home_view =
        stage::get_json(&format!("{}/api/kernel/away", stage.base_url()), None).await?.1;
    report.check(
        "the web home keeps its own place: it still has the night to read",
        home_view["lines"].as_array().is_some_and(|lines| lines.len() == away.lines.len()),
        format!("{home_view}"),
    );
    let record = stage.client().await?.record(Some(500)).await?;
    let on_record = record.iter().find(|line| line.target.as_deref() == Some(decision.id.as_str()));
    if let Some(line) = on_record {
        report.note(&format!(
            "the record: {} of the decision, by the person who paired the phone",
            line.action
        ));
    }
    report.check(
        "the phone's answer is on record",
        on_record.is_some(),
        "no record line names the decision",
    );

    drop(kernel);
    let none_left =
        until(Duration::from_secs(5), || async { Ok(!stage::running(&instance)) }).await?;
    report.check(
        "no kernel or driver process is left running",
        none_left,
        "the mail driver still runs",
    );
    Ok(())
}

/// Whether `process` is a run of the routine `routine`: the routine itself or a firing it
/// spawned.
fn run_of(process: &adk_kernel::Process, routine: &str) -> bool {
    process.meta.pid == routine || process.meta.parent.as_deref() == Some(routine)
}

/// Whether a run of `routine` has finished.
async fn finished(client: &ControlClient, routine: &str) -> Result<bool> {
    Ok(client.processes(&ProcessesQuery::default()).await?.iter().any(|process| {
        run_of(process, routine)
            && matches!(process.run.state, RunState::Completed | RunState::Failed)
    }))
}

/// `at` rounded up to the next whole minute.
fn whole_minute(at: DateTime<Utc>) -> DateTime<Utc> {
    use chrono::{DurationRound, TimeDelta};
    let floor = at.duration_trunc(TimeDelta::minutes(1)).unwrap_or(at);
    if floor == at { at } else { floor + TimeDelta::minutes(1) }
}

/// The quiet windows the home reads for an area: the person's and the workspace's.
fn quiet_windows(area: &AreaView) -> Vec<QuietHours> {
    area.quiet_time.iter().chain(&area.also_quiet).cloned().collect()
}

/// Whether the home shows the morning briefing at `now` (spec 3.3 §5.1, §12 phase 2), by its
/// own rule (`briefingDue` in `adk-surface/web/src/away.ts`): no quiet window holds, one ended
/// since the person last marked the page seen, and the page has something to say.
fn briefing_due(page: &AwayResponse, windows: &[QuietHours], now: DateTime<Utc>) -> bool {
    if windows.iter().any(|window| window.contains(now))
        || (page.lines.is_empty() && page.waiting == 0)
    {
        return false;
    }
    // A window ended since `since`: some second between then and now was quiet.
    let span = (now - page.since).num_seconds().clamp(0, 2 * 86_400);
    (1..=span).any(|back| {
        let at = now - TimeDelta::seconds(back);
        windows.iter().any(|window| window.contains(at))
    })
}

/// Whether a briefing has nothing to say.
fn is_quiet(away: &AwayResponse) -> bool {
    away.lines.is_empty() && away.counts == AwayCounts::default() && away.spent_micro_usd == 0
}
