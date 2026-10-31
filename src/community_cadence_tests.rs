use super::tests::{pending_fixture, source_sample};
use super::*;

fn adaptive(base: u32) -> CommunitySettings {
    CommunitySettings {
        enabled: true,
        target_interval_seconds: base,
        cadence_mode: CommunityCadenceMode::Adaptive,
    }
}
fn observed(population: u64, rows: u64) -> Option<SampleCounts> {
    Some(SampleCounts {
        reported_chatters: Some(population),
        viewer_rows: rows,
    })
}

#[test]
fn cadence_examples_fallbacks_and_integer_extremes_are_explicit() {
    for (population, expected) in [
        (100, 300),
        (279, 108),
        (464, 65),
        (1_000, 30),
        (100_000, 30),
    ] {
        assert_eq!(
            cadence(adaptive(300), observed(population, 100)),
            CadenceDecision {
                seconds: expected,
                basis: CadenceBasis::Adaptive
            }
        );
    }
    for (counts, basis) in [
        (None, CadenceBasis::NoSample),
        (
            Some(SampleCounts {
                reported_chatters: None,
                viewer_rows: 100,
            }),
            CadenceBasis::MissingReportedCount,
        ),
        (observed(0, 0), CadenceBasis::ZeroReportedCount),
        (observed(100, 0), CadenceBasis::NoViewerRows),
        (observed(99, 100), CadenceBasis::InconsistentCounts),
    ] {
        assert_eq!(
            cadence(adaptive(300), counts),
            CadenceDecision {
                seconds: 300,
                basis
            }
        );
    }
    assert_eq!(
        cadence(adaptive(86_400), observed(u64::MAX, u64::MAX)).seconds,
        86_400
    );
    assert_eq!(cadence(adaptive(86_400), observed(u64::MAX, 1)).seconds, 30);
    assert_eq!(
        cadence(
            CommunitySettings {
                cadence_mode: CommunityCadenceMode::Fixed,
                ..adaptive(300)
            },
            observed(100_000, 100)
        ),
        CadenceDecision {
            seconds: 300,
            basis: CadenceBasis::Fixed
        }
    );
}

#[test]
fn targets_are_bounded_and_monotone_in_reported_size() {
    for base in [30, 31, 300, 86_400] {
        let mut previous = base;
        for population in 100..10_000 {
            let target = cadence(adaptive(base), observed(population, 100)).seconds;
            assert!((30..=base).contains(&target));
            assert!(target <= previous);
            previous = target;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn mixed_populations_do_not_starve_small_chats_under_serial_overload() {
    let now = Instant::now();
    let mut schedule = BTreeMap::new();
    let mut visits = BTreeMap::new();
    for index in 0..50 {
        let channel = format!("room{index:02}");
        let mut entry = Schedule::new(now);
        entry.counts = observed(if index % 2 == 0 { 100 } else { 100_000 }, 100);
        schedule.insert(channel.clone(), entry);
        visits.insert(channel, Vec::new());
    }
    let mut next_global = now;
    let mut previous_completion = None;
    for job in 0..150 {
        let eligible = schedule
            .values()
            .map(Schedule::eligible)
            .min()
            .unwrap()
            .max(next_global);
        let channel = next_channel(&schedule, eligible).unwrap();
        if let Some(completed) = previous_completion {
            assert!(eligible >= completed + START_GAP);
        }
        visits.get_mut(&channel).unwrap().push(job);
        let entry = schedule.get_mut(&channel).unwrap();
        entry.last_attempt = Some(eligible);
        entry.cadence_due =
            eligible + Duration::from_secs(cadence(adaptive(300), entry.counts).seconds.into());
        let completed = eligible + Duration::from_secs(5);
        next_global = completed + START_GAP;
        previous_completion = Some(completed);
    }
    for turns in visits.values() {
        assert!(
            turns.len() >= 2,
            "Every selected room must receive repeated turns"
        );
        assert!(turns.windows(2).all(|pair| pair[1] - pair[0] <= 100));
    }
}

#[tokio::test]
async fn only_durable_success_updates_counts_and_uses_the_attempt_anchor() {
    let (_directory, store, mut runner) = pending_fixture().await;
    runner.settings = adaptive(300);
    let anchor = Instant::now() - Duration::from_secs(10);
    runner.pending.as_mut().unwrap().started = anchor;
    let mut sample = source_sample();
    sample.reported_chatters = Some(279);
    sample.category_counts[5] = 100;
    let before = Instant::now();
    runner
        .event(HostEvent::SampleCompleted {
            job_id: 1,
            channel: "example".into(),
            outcome: Ok(sample),
        })
        .await
        .unwrap();
    let entry = &runner.schedule["example"];
    assert_eq!(entry.cadence_due, anchor + Duration::from_secs(108));
    assert_eq!(entry.counts.unwrap().reported_chatters, Some(279));
    assert_eq!(entry.counts.unwrap().viewer_rows, 100);
    assert!(runner.next_start >= before + START_GAP);
    assert_eq!(store.community_stats().await.unwrap().successful_count, 1);
    runner
        .retire(CommunityRunEndReason::Shutdown)
        .await
        .unwrap();

    let (_directory, store, mut runner) = pending_fixture().await;
    runner.schedule.get_mut("example").unwrap().counts = observed(464, 100);
    store.call_write(crate::diagnostics::Operation::CommunityAppend, |conn| {
        conn.execute_batch("CREATE TEMP TRIGGER reject_success_fixture BEFORE UPDATE OF outcome ON community_samples WHEN NEW.outcome='success' BEGIN SELECT RAISE(ABORT,'fixture success failure'); END;")?;
        Ok(())
    }).await.unwrap();
    assert!(runner
        .event(HostEvent::SampleCompleted {
            job_id: 1,
            channel: "example".into(),
            outcome: Ok(source_sample())
        })
        .await
        .is_err());
    assert_eq!(
        runner.schedule["example"].counts.unwrap().reported_chatters,
        Some(464)
    );
    assert_eq!(store.community_stats().await.unwrap().member_count, 0);
}

#[tokio::test]
async fn failed_and_cancelled_responses_cannot_replace_adaptive_inputs() {
    for cancelled in [false, true] {
        let (_directory, _store, mut runner) = pending_fixture().await;
        runner.schedule.get_mut("example").unwrap().counts = observed(464, 100);
        runner.pending.as_mut().unwrap().cancelled = cancelled;
        let outcome = if cancelled {
            Ok(source_sample())
        } else {
            Err(community_source::host::SourceFailure {
                code: "navigation_ui_deadline",
                pauses_community: false,
            })
        };
        runner
            .event(HostEvent::SampleCompleted {
                job_id: 1,
                channel: "example".into(),
                outcome,
            })
            .await
            .unwrap();
        assert_eq!(
            runner.schedule["example"].counts.unwrap().reported_chatters,
            Some(464)
        );
        runner
            .retire(CommunityRunEndReason::Shutdown)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn changing_settings_preserves_overdue_work_backoff_and_denial_pause() {
    let (_directory, store, mut runner) = pending_fixture().await;
    let now = Instant::now();
    let overdue = now - Duration::from_secs(100);
    let entry = runner.schedule.get_mut("example").unwrap();
    entry.last_attempt = Some(now - Duration::from_secs(200));
    entry.cadence_due = overdue;
    entry.counts = observed(464, 100);
    runner
        .finish_failure(CommunityFailureCode::Timeout, false)
        .await
        .unwrap();
    let retry = runner.schedule["example"].retry_not_before;
    runner.pause_source("integrity_denied").await.unwrap();
    for settings in [
        CommunitySettings {
            cadence_mode: CommunityCadenceMode::Fixed,
            ..adaptive(86_400)
        },
        adaptive(30),
    ] {
        runner
            .control(Command::Settings(settings.into()))
            .await
            .unwrap();
        let entry = &runner.schedule["example"];
        assert!(entry.cadence_due <= overdue);
        assert_eq!(entry.retry_not_before, retry);
        assert!(next_channel(&runner.schedule, retry - Duration::from_millis(1)).is_none());
        assert!(runner.paused);
        assert_eq!(
            store.community_pause().await.unwrap(),
            Some(CommunityPauseReason::IntegrityDenied)
        );
    }
    runner
        .retire(CommunityRunEndReason::Shutdown)
        .await
        .unwrap();
}

#[tokio::test]
async fn queued_legacy_updates_preserve_the_mode_current_at_dequeue() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("test.sqlite3"))
        .await
        .unwrap();
    let profile = directory.path().join("profile");
    let (service, runner) = Community::initialize(store.clone(), profile.clone())
        .await
        .unwrap();
    let mut responses = Vec::new();
    for (mode, base) in [
        (Some(CommunityCadenceMode::Fixed), 60),
        (None, 120),
        (Some(CommunityCadenceMode::Adaptive), 300),
        (None, 600),
    ] {
        let (reply, response) = oneshot::channel();
        assert!(service
            .commands
            .try_send((
                Command::Settings(CommunitySettingsUpdate {
                    enabled: false,
                    target_interval_seconds: base,
                    cadence_mode: mode
                }),
                reply
            ))
            .is_ok());
        responses.push(response);
    }
    let (_selected, selected) = watch::channel(Vec::new());
    let (stop, shutdown) = watch::channel(false);
    let task = tokio::spawn(runner.run(selected, shutdown));
    let mut received = Vec::new();
    for response in responses {
        let ControlReply::Settings(settings) = response.await.unwrap().unwrap() else {
            panic!("resolved settings reply");
        };
        received.push(settings);
    }
    assert_eq!(
        received
            .iter()
            .map(|settings| settings.cadence_mode)
            .collect::<Vec<_>>(),
        [
            CommunityCadenceMode::Fixed,
            CommunityCadenceMode::Fixed,
            CommunityCadenceMode::Adaptive,
            CommunityCadenceMode::Adaptive
        ]
    );
    assert_eq!(
        store
            .community_settings()
            .await
            .unwrap()
            .target_interval_seconds,
        600
    );
    stop.send_replace(true);
    task.await.unwrap().unwrap();
    assert!(!profile.exists());
    let legacy: CommunitySettingsUpdate =
        serde_json::from_value(json!({"enabled":false,"target_interval_seconds":300})).unwrap();
    assert!(legacy.cadence_mode.is_none());
    assert!(serde_json::from_value::<CommunitySettingsUpdate>(
        json!({"enabled":false,"target_interval_seconds":300,"cadence_mode":"invalid"})
    )
    .is_err());
}
