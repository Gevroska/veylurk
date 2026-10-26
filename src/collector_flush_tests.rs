//! Deterministic timer and size contracts for the production pending-receipt path.
use super::*;
use futures_util::FutureExt;
use tokio::time::{advance, Instant, Interval, MissedTickBehavior};

fn flush_interval() -> Interval {
    let mut flush = tokio::time::interval(Duration::from_millis(250));
    flush.set_missed_tick_behavior(MissedTickBehavior::Skip);
    flush
}

fn receipt(index: usize) -> NewObservation {
    NewObservation {
        channel: "test".into(),
        account: Some(format!("account{index}")),
        kind: "join".into(),
        received_at: format!("2026-10-26T00:00:00.{index:03}Z"),
    }
}

#[tokio::test(start_paused = true)]
async fn first_pending_receipt_gets_full_age_budget_after_startup_or_idle() {
    for idle in [Duration::ZERO, Duration::from_secs(13)] {
        let mut flush = flush_interval();
        let mut pending = Vec::new();
        // The collector does not poll the interval while its buffer is empty.
        advance(idle).await;
        let first_pending = Instant::now();
        assert!(!queue_observations(
            &mut flush,
            &mut pending,
            vec![receipt(0)]
        ));
        assert!(flush.tick().now_or_never().is_none());
        advance(Duration::from_millis(249)).await;
        assert!(flush.tick().now_or_never().is_none());
        advance(Duration::from_millis(1)).await;
        assert_eq!(
            flush.tick().await,
            first_pending + Duration::from_millis(250)
        );
        assert_eq!(first_pending.elapsed(), Duration::from_millis(250));
    }
}

#[tokio::test(start_paused = true)]
async fn later_receipts_keep_the_first_pending_deadline_and_receipt_order() {
    let mut flush = flush_interval();
    let mut pending = Vec::new();
    let first_pending = Instant::now();
    assert!(!queue_observations(
        &mut flush,
        &mut pending,
        vec![receipt(0)]
    ));
    advance(Duration::from_millis(200)).await;
    assert!(!queue_observations(
        &mut flush,
        &mut pending,
        vec![receipt(1)]
    ));
    advance(Duration::from_millis(49)).await;
    assert!(flush.tick().now_or_never().is_none());
    advance(Duration::from_millis(1)).await;
    assert_eq!(
        flush.tick().await,
        first_pending + Duration::from_millis(250)
    );
    assert_eq!(first_pending.elapsed(), Duration::from_millis(250));
    assert_eq!(pending.len(), 2);
    for (index, event) in pending.iter().enumerate() {
        let expected = receipt(index);
        assert_eq!(event.account, expected.account);
        assert_eq!(event.received_at, expected.received_at);
    }
}

#[tokio::test(start_paused = true)]
async fn later_receipt_cannot_renew_an_already_overdue_pending_batch() {
    let mut flush = flush_interval();
    let mut pending = Vec::new();
    let first_pending = Instant::now();
    assert!(!queue_observations(
        &mut flush,
        &mut pending,
        vec![receipt(0)]
    ));
    advance(Duration::from_millis(300)).await;
    assert!(!queue_observations(
        &mut flush,
        &mut pending,
        vec![receipt(1)]
    ));
    assert_eq!(
        flush.tick().await,
        first_pending + Duration::from_millis(250)
    );
    assert_eq!(first_pending.elapsed(), Duration::from_millis(300));
}

#[tokio::test(start_paused = true)]
async fn full_batch_is_ready_immediately_and_slow_flush_leaves_next_batch_a_fresh_deadline() {
    let mut flush = flush_interval();
    let mut pending = Vec::new();
    let first_pending = Instant::now();
    assert!(!queue_observations(
        &mut flush,
        &mut pending,
        (0..511).map(receipt).collect()
    ));
    assert!(queue_observations(
        &mut flush,
        &mut pending,
        vec![receipt(511)]
    ));
    assert_eq!(pending.len(), 512);
    assert_eq!(first_pending.elapsed(), Duration::ZERO);
    assert!(flush.tick().now_or_never().is_none());

    // flush_pending drains the vector before awaiting the durable append. Model
    // a slow completed flush without filesystem work or a real thirteen-second wait.
    pending.clear();
    advance(Duration::from_secs(13)).await;
    let next_pending = Instant::now();
    assert!(!queue_observations(
        &mut flush,
        &mut pending,
        vec![receipt(512)]
    ));
    assert!(flush.tick().now_or_never().is_none());
    advance(Duration::from_millis(249)).await;
    assert!(flush.tick().now_or_never().is_none());
    advance(Duration::from_millis(1)).await;
    assert_eq!(
        flush.tick().await,
        next_pending + Duration::from_millis(250)
    );
    assert_eq!(next_pending.elapsed(), Duration::from_millis(250));
}
