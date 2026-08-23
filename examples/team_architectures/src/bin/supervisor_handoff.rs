//! Supervisor handoff to one of two exact specialist targets.

use team_architectures_example::{print_spec, run_team, supervisor_handoff_team};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (spec, team) = supervisor_handoff_team()?;
    print_spec(&spec)?;
    println!(
        "Expected: supervisor hands off to billing; billing cannot hand off to technical or back."
    );
    run_team(
        team,
        "team-supervisor-handoff",
        "My latest invoice contains the same subscription charge twice. What should I do?",
    )
    .await
}
