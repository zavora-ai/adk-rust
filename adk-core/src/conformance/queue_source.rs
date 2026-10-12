//! Conformance cases for [`QueueSource`] implementations.
//!
//! | Case | Contract |
//! |------|----------|
//! | [`pull_leases_and_ack_removes`] | A pulled message carries its id, body, and a receipt; it is not pulled again while leased; acking removes it for good |
//! | [`nack_redelivers_with_a_new_receipt`] | A nacked message is redelivered with the same id, a new receipt, and a higher delivery count; the old receipt no longer acks |
//! | [`lapsed_lease_redelivers`] | A message neither acked nor nacked is redelivered once its lease lapses, and the lapsed receipt is refused |
//! | [`pull_respects_the_batch_size`] | A pull never returns more than `max_messages` |
//! | [`duplicate_publishes_are_both_delivered`] | Two publishes of one id are two deliveries with that id; deduplication is the consumer's job |

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::unique;
use crate::queue_source::{QUEUE_UNKNOWN_RECEIPT_CODE, QueueMessage, QueueSource};

/// Opens a [`QueueSource`] for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use std::time::Duration;
/// use adk_core::conformance::QueueSourceHarness;
/// use adk_core::{Clock, InMemoryQueueSource, ManualClock, QueueSource, async_trait};
///
/// struct InMemory {
///     clock: ManualClock,
///     source: Arc<InMemoryQueueSource>,
/// }
///
/// #[async_trait]
/// impl QueueSourceHarness for InMemory {
///     async fn open(&self) -> Arc<dyn QueueSource> {
///         self.source.clone()
///     }
///
///     async fn publish(&self, id: &str, body: &[u8]) {
///         self.source.publish(id, body.to_vec());
///     }
///
///     async fn lapse_leases(&self) {
///         self.clock.advance(Duration::from_secs(31));
///     }
/// }
/// ```
#[async_trait]
pub trait QueueSourceHarness: Send + Sync {
    /// Opens the source. Every call in one case sees the same queue.
    async fn open(&self) -> Arc<dyn QueueSource>;

    /// Publishes a message with `id` and `body`.
    async fn publish(&self, id: &str, body: &[u8]);

    /// Lets every current lease lapse: advance a manual clock, or wait out the
    /// visibility timeout of a live broker.
    async fn lapse_leases(&self);

    /// How long a pull waits for messages that were just published.
    fn pull_wait(&self) -> Duration {
        Duration::from_millis(500)
    }
}

async fn pull_ids<H: QueueSourceHarness + ?Sized>(
    harness: &H,
    source: &dyn QueueSource,
    ids: &[String],
) -> Vec<QueueMessage> {
    let mut found = Vec::new();
    for _ in 0..20 {
        let batch = source.pull(10, harness.pull_wait()).await.expect("pull");
        found.extend(batch.into_iter().filter(|message| ids.contains(&message.id)));
        if found.len() >= ids.len() {
            break;
        }
    }
    found
}

#[track_caller]
fn assert_unknown_receipt(result: crate::Result<()>, context: &str) {
    let error = result.expect_err(context);
    assert_eq!(error.code, QUEUE_UNKNOWN_RECEIPT_CODE, "{context}: {error}");
}

/// A pulled message is leased, and acking removes it.
pub async fn pull_leases_and_ack_removes<H: QueueSourceHarness + ?Sized>(harness: &H) {
    let source = harness.open().await;
    let id = unique("ack");
    harness.publish(&id, br#"{"n":1}"#).await;
    let pulled = pull_ids(harness, source.as_ref(), std::slice::from_ref(&id)).await;
    assert_eq!(pulled.len(), 1, "the message is delivered once");
    let message = &pulled[0];
    assert_eq!((message.body.as_slice(), message.delivery_count), (br#"{"n":1}"#.as_slice(), 1));
    let again = source.pull(10, Duration::ZERO).await.expect("pull");
    assert!(again.iter().all(|other| other.id != id), "a leased message is not pulled again");
    source.ack(&message.receipt).await.expect("ack");
    harness.lapse_leases().await;
    let after = source.pull(10, Duration::ZERO).await.expect("pull");
    assert!(after.iter().all(|other| other.id != id), "an acked message never returns");
    assert_unknown_receipt(source.ack(&message.receipt).await, "a second ack");
}

/// A nacked message is redelivered under a new receipt.
pub async fn nack_redelivers_with_a_new_receipt<H: QueueSourceHarness + ?Sized>(harness: &H) {
    let source = harness.open().await;
    let id = unique("nack");
    harness.publish(&id, b"{}").await;
    let first = pull_ids(harness, source.as_ref(), std::slice::from_ref(&id)).await.remove(0);
    source.nack(&first.receipt, Duration::ZERO).await.expect("nack");
    let second = pull_ids(harness, source.as_ref(), std::slice::from_ref(&id)).await.remove(0);
    assert_eq!(second.id, first.id);
    assert_ne!(second.receipt, first.receipt, "a redelivery has a new receipt");
    assert!(second.delivery_count > first.delivery_count, "the delivery count rises");
    assert_unknown_receipt(source.ack(&first.receipt).await, "the old receipt");
    source.ack(&second.receipt).await.expect("ack");
}

/// A lapsed lease redelivers the message.
pub async fn lapsed_lease_redelivers<H: QueueSourceHarness + ?Sized>(harness: &H) {
    let source = harness.open().await;
    let id = unique("lapse");
    harness.publish(&id, b"{}").await;
    let first = pull_ids(harness, source.as_ref(), std::slice::from_ref(&id)).await.remove(0);
    harness.lapse_leases().await;
    let second = pull_ids(harness, source.as_ref(), std::slice::from_ref(&id)).await.remove(0);
    assert_ne!(second.receipt, first.receipt);
    assert_unknown_receipt(source.ack(&first.receipt).await, "the lapsed receipt");
    source.ack(&second.receipt).await.expect("ack");
}

/// A pull returns at most `max_messages`.
pub async fn pull_respects_the_batch_size<H: QueueSourceHarness + ?Sized>(harness: &H) {
    let source = harness.open().await;
    let ids: Vec<String> = (0..5).map(|n| unique(&format!("batch-{n}"))).collect();
    for id in &ids {
        harness.publish(id, b"{}").await;
    }
    let batch = source.pull(2, harness.pull_wait()).await.expect("pull");
    assert!(batch.len() <= 2, "pulled {} with max 2", batch.len());
    let mut seen: Vec<QueueMessage> = batch;
    seen.extend(pull_ids(harness, source.as_ref(), &ids).await);
    for message in seen.iter().filter(|message| ids.contains(&message.id)) {
        source.ack(&message.receipt).await.expect("ack");
    }
}

/// Duplicate publishes are delivered as separate messages with one id.
pub async fn duplicate_publishes_are_both_delivered<H: QueueSourceHarness + ?Sized>(harness: &H) {
    let source = harness.open().await;
    let id = unique("dup");
    harness.publish(&id, b"{}").await;
    harness.publish(&id, b"{}").await;
    let pulled = pull_ids(harness, source.as_ref(), &[id.clone(), id.clone()]).await;
    assert_eq!(pulled.len(), 2, "both copies are delivered");
    assert_ne!(pulled[0].receipt, pulled[1].receipt);
    for message in pulled {
        source.ack(&message.receipt).await.expect("ack");
    }
}
