# Cron Scheduling Example

Exercises the cron scheduling API from `adk-server`: create jobs, list them, and pause or delete them.

## What This Shows

- **`CronState` / `ScheduleStore`** — cron jobs stored as schedules (in memory here) and the `DurableScheduler` loop
- **Concurrency policies** — `skip`, `allow`, and `queue`
- **`POST/GET/PATCH/DELETE /cron`** — the job management surface

## Prerequisites

- **Rust 1.95+** (edition 2024)
- Built with the adk-server `background` feature (already enabled in this example's manifest)

## Run

```bash
cargo run --manifest-path examples/cron_scheduling/Cargo.toml
```
