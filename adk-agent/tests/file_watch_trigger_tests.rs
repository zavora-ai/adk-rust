//! A dropped `FileWatchTrigger` subscription must stop its watcher.
//!
//! `subscribe` used to park the thread holding the `notify` watcher forever, so every
//! subscription a long-lived process dropped kept a thread, and the watcher's own thread,
//! watching its directory until the process exited.

#![cfg(feature = "ambient")]

use std::time::Duration;

use adk_agent::ambient::{EventSource, FileWatchTrigger};
use futures::StreamExt;

/// The process's thread count, where the platform lists threads.
#[cfg(target_os = "linux")]
fn thread_count() -> Option<usize> {
    std::fs::read_dir("/proc/self/task").ok().map(Iterator::count)
}

/// The process's thread count, where the platform lists threads.
#[cfg(target_os = "macos")]
fn thread_count() -> Option<usize> {
    let output = std::process::Command::new("ps")
        .args(["-M", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    // One header line, then one line per thread.
    Some(String::from_utf8_lossy(&output.stdout).lines().count().saturating_sub(1))
}

/// The process's thread count, where the platform lists threads.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn thread_count() -> Option<usize> {
    None
}

/// Subscribes, proves the watcher delivers a change, and drops the subscription.
async fn subscribe_and_drop(trigger: &FileWatchTrigger, dir: &std::path::Path, round: usize) {
    let mut events = trigger.subscribe().await.unwrap();
    // Let the watcher register before the change.
    tokio::time::sleep(Duration::from_millis(200)).await;
    std::fs::write(dir.join(format!("{round}.txt")), "changed").unwrap();
    let event = tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .expect("the watcher delivers a change while subscribed")
        .expect("the stream is open while subscribed");
    assert!(event.payload["path"].as_str().unwrap().ends_with(".txt"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_a_subscription_stops_its_watcher_thread() {
    let dir = tempfile::tempdir().unwrap();
    let trigger = FileWatchTrigger::new(dir.path(), "*.txt").unwrap();
    // One round first, so threads the platform creates once per process exist before the
    // baseline.
    subscribe_and_drop(&trigger, dir.path(), 0).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let Some(baseline) = thread_count() else {
        return;
    };

    const ROUNDS: usize = 6;
    for round in 1..=ROUNDS {
        subscribe_and_drop(&trigger, dir.path(), round).await;
    }

    // Each leaked subscription keeps at least its own thread; allow two for threads the
    // platform pools on its own schedule.
    let started = tokio::time::Instant::now();
    loop {
        let threads = thread_count().unwrap();
        if threads <= baseline + 2 {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{} threads above the baseline of {baseline} five seconds after {ROUNDS} dropped \
             subscriptions",
            threads - baseline
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
