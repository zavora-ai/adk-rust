//! The bounded queue between a source's producer and its consumer: when the consumer falls
//! behind, the oldest item goes, so the consumer always sees the newest frames.

#![cfg_attr(
    not(all(
        feature = "rtsp",
        feature = "ffmpeg",
        feature = "screen",
        feature = "browser",
        feature = "surface-camera"
    )),
    allow(
        dead_code,
        reason = "each part serves some of the sources; a build with only some of them leaves the rest unused"
    )
)]

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::FrameStream;
use crate::error::{Result, VisionError};
use crate::media::Frame;

struct State<T> {
    items: VecDeque<T>,
    closed: bool,
    dropped: u64,
}

struct Shared<T> {
    state: Mutex<State<T>>,
    ready: Notify,
    /// Cancelled when the receiver is dropped, so the producer stops.
    gone: CancellationToken,
    capacity: usize,
}

impl<T> Shared<T> {
    fn lock(&self) -> std::sync::MutexGuard<'_, State<T>> {
        // A poisoned lock means a producer panicked mid-push; the queue itself is still
        // consistent, since every critical section leaves it whole.
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// What a push did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pushed {
    /// Queued.
    Queued,
    /// Queued, after the oldest item was dropped to make room.
    DroppedOldest,
    /// The receiver is gone; nothing was queued.
    Closed,
}

/// The producer's half.
pub(crate) struct QueueSender<T> {
    shared: Arc<Shared<T>>,
}

/// The consumer's half.
pub(crate) struct QueueReceiver<T> {
    shared: Arc<Shared<T>>,
}

/// A queue holding at most `capacity` items.
pub(crate) fn queue<T>(capacity: usize) -> (QueueSender<T>, QueueReceiver<T>) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State { items: VecDeque::new(), closed: false, dropped: 0 }),
        ready: Notify::new(),
        gone: CancellationToken::new(),
        capacity: capacity.max(1),
    });
    (QueueSender { shared: Arc::clone(&shared) }, QueueReceiver { shared })
}

impl<T> QueueSender<T> {
    /// Queues `item`, dropping the oldest when the queue is full.
    pub(crate) fn push(&self, item: T) -> Pushed {
        if self.shared.gone.is_cancelled() {
            return Pushed::Closed;
        }
        let pushed = {
            let mut state = self.shared.lock();
            let pushed = if state.items.len() >= self.shared.capacity {
                state.items.pop_front();
                state.dropped += 1;
                Pushed::DroppedOldest
            } else {
                Pushed::Queued
            };
            state.items.push_back(item);
            pushed
        };
        self.shared.ready.notify_one();
        pushed
    }

    /// Drops every queued item: a camera switched off keeps none of its frames.
    pub(crate) fn clear(&self) {
        self.shared.lock().items.clear();
    }

    /// Items dropped so far because the consumer fell behind.
    pub(crate) fn dropped(&self) -> u64 {
        self.shared.lock().dropped
    }

    /// Cancelled when the receiver is gone.
    pub(crate) fn gone(&self) -> CancellationToken {
        self.shared.gone.clone()
    }
}

impl<T> Drop for QueueSender<T> {
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.ready.notify_one();
    }
}

impl<T> QueueReceiver<T> {
    /// The next item, or `None` once the sender is gone and the queue is empty.
    pub(crate) async fn recv(&self) -> Option<T> {
        loop {
            {
                let mut state = self.shared.lock();
                if let Some(item) = state.items.pop_front() {
                    return Some(item);
                }
                if state.closed {
                    return None;
                }
            }
            // `notify_one` stores a permit when no one waits, so a push between the unlock
            // and this await is not lost.
            self.shared.ready.notified().await;
        }
    }
}

impl<T> Drop for QueueReceiver<T> {
    fn drop(&mut self) {
        self.shared.gone.cancel();
    }
}

impl QueueSender<Result<Frame>> {
    /// Queues an error and ends the stream: no frame is dropped for it, and nothing follows
    /// it.
    pub(crate) fn fail(self, error: VisionError) {
        {
            let mut state = self.shared.lock();
            if state.items.len() >= self.shared.capacity {
                state.items.pop_front();
            }
            state.items.push_back(Err(error));
        }
        self.shared.ready.notify_one();
    }
}

/// A frame stream fed by `produce`, which runs on its own task until it returns or the
/// stream is dropped; dropping the stream drops `produce`'s future, and with it any
/// connection or child process it holds.
pub(crate) fn spawn_frames<F, Fut>(capacity: usize, produce: F) -> FrameStream
where
    F: FnOnce(QueueSender<Result<Frame>>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = queue(capacity);
    let gone = tx.gone();
    let work = produce(tx);
    tokio::spawn(async move {
        tokio::select! {
            () = gone.cancelled() => {}
            () = work => {}
        }
    });
    futures::stream::unfold(rx, |rx| async move { rx.recv().await.map(|item| (item, rx)) }).boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_full_queue_drops_the_oldest() {
        let (tx, rx) = queue(2);
        assert_eq!(tx.push(1), Pushed::Queued);
        assert_eq!(tx.push(2), Pushed::Queued);
        assert_eq!(tx.push(3), Pushed::DroppedOldest);
        assert_eq!(tx.dropped(), 1);
        drop(tx);
        assert_eq!(rx.recv().await, Some(2));
        assert_eq!(rx.recv().await, Some(3));
        assert_eq!(rx.recv().await, None);
    }

    #[tokio::test]
    async fn dropping_the_receiver_stops_the_producer() {
        let (tx, rx) = queue::<u32>(2);
        let gone = tx.gone();
        drop(rx);
        assert!(gone.is_cancelled());
        assert_eq!(tx.push(1), Pushed::Closed);
    }

    #[tokio::test]
    async fn a_waiting_receiver_wakes_on_push() {
        let (tx, rx) = queue(1);
        let waiter = tokio::spawn(async move { rx.recv().await });
        tokio::task::yield_now().await;
        tx.push(7u8);
        assert_eq!(waiter.await.unwrap(), Some(7));
    }
}
