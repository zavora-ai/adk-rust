//! Per-turn framework work must not grow with the parts of a session a turn does not use.
//!
//! Bytes allocated per turn are counted on the test's thread, so the measure is deterministic
//! where timings are not: a turn that copies the whole session allocates in proportion to it.

use adk_bench::{LongHistoryBench, LongHistoryConfig};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// The system allocator, counting the bytes each thread asks for.
struct ThreadCounting;

thread_local! {
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
}

fn count(bytes: usize) {
    // A thread being torn down has no counter left; its bytes are not measured.
    let _ = ALLOCATED.try_with(|allocated| allocated.set(allocated.get() + bytes));
}

// SAFETY: every call forwards to `System` with the caller's layout and pointer unchanged.
unsafe impl GlobalAlloc for ThreadCounting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: ThreadCounting = ThreadCounting;

fn allocated() -> usize {
    ALLOCATED.with(Cell::get)
}

/// Bytes allocated by each of `turns` turns.
async fn bytes_per_turn(config: &LongHistoryConfig, turns: usize) -> Vec<usize> {
    let bench = LongHistoryBench::new(config).await.unwrap();
    let mut bytes = Vec::with_capacity(turns);
    for _ in 0..turns {
        let before = allocated();
        bench.turn().await.unwrap();
        bytes.push(allocated() - before);
    }
    bytes
}

fn mean(bytes: &[usize]) -> usize {
    bytes.iter().sum::<usize>() / bytes.len()
}

/// With `history_max_events`, a turn loads and copies only the window, so turn 300 costs what
/// turn 100 does although the session holds three times the events.
#[tokio::test]
async fn a_history_window_keeps_the_cost_of_a_turn_flat() {
    let config = LongHistoryConfig { history_max_events: Some(40), ..LongHistoryConfig::default() };
    let bytes = bytes_per_turn(&config, 300).await;

    let (early, late) = (mean(&bytes[50..100]), mean(&bytes[250..300]));
    assert!(
        late * 10 < early * 12,
        "turns 250-300 allocated {late} bytes each, turns 50-100 {early}: the window is not \
         bounding the work"
    );
}

/// Without a window, each turn copies the history it loads and sends the model. Every stored
/// event adds about 7 KB to each later turn (one copy into the invocation, and the history of
/// each of the turn's two model calls); one more full copy of the history per turn, as the
/// runner made before it moved the loaded events, adds about 2.5 KB.
#[tokio::test]
async fn a_turn_copies_the_history_a_bounded_number_of_times() {
    let bytes = bytes_per_turn(&LongHistoryConfig::default(), 300).await;

    // Four events per turn: the growth per turn over four is the cost per stored event.
    let (early, late) = (mean(&bytes[50..100]), mean(&bytes[250..300]));
    let per_event = (late - early) / (200 * 4);
    assert!(
        per_event < 8 * 1024,
        "each stored event adds {per_event} bytes to every later turn; a turn copies the \
         history more often than it did"
    );
}
