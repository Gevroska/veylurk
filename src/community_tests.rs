use super::*;
use axum::{body::Body, http::Request};
use tower::ServiceExt;

#[test]
fn coordinator_keeps_the_worker_terminal_consumer_alive_beyond_all_worker_exit_barriers() {
    let budget = community_source::host::MAX_WORKER_RETIREMENT;
    assert!(budget + Duration::from_secs(1) <= CLEANUP_TIMEOUT);
    assert!(budget < CONTROL_REPLY_TIMEOUT);
}

#[tokio::test(start_paused = true)]
async fn serial_schedule_is_fair_and_backoff_does_not_block_other_rooms() {
    let now = Instant::now();
    let mut schedule = BTreeMap::new();
    for channel in ["alpha", "bravo", "charlie"] {
        schedule.insert(channel.to_owned(), Schedule::new(now));
    }
    for (offset, expected) in [(0, "alpha"), (30, "bravo"), (60, "charlie")] {
        let at = now + Duration::from_secs(offset);
        assert_eq!(next_channel(&schedule, at).as_deref(), Some(expected));
        schedule.get_mut(expected).unwrap().cadence_due = at + Duration::from_secs(300);
    }
    assert!(next_channel(&schedule, now + Duration::from_secs(90)).is_none());
    schedule.get_mut("alpha").unwrap().cadence_due = now + Duration::from_secs(1800);
    assert_eq!(
        next_channel(&schedule, now + Duration::from_secs(330)).as_deref(),
        Some("bravo")
    );
}

#[tokio::test]
async fn disabled_startup_and_shutdown_do_not_create_a_browser_profile() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("test.sqlite3"))
        .await
        .unwrap();
    let profile = directory.path().join("never-created");
    let (service, runner) = Community::initialize(store.clone(), profile.clone())
        .await
        .unwrap();
    let (_selected, selected) = watch::channel(vec!["example".to_owned()]);
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(runner.run(selected, stopped));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(service.status.read().await.state, "disabled");
    assert!(!profile.exists());
    stop.send_replace(true);
    task.await.unwrap().unwrap();
    assert!(!profile.exists());
    assert_eq!(store.community_stats().await.unwrap().run_count, 0);
}

#[tokio::test]
async fn community_routes_inherit_localhost_write_protections_and_validate_queries() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("test.sqlite3"))
        .await
        .unwrap();
    let (service, _runner) = Community::initialize(store.clone(), directory.path().join("profile"))
        .await
        .unwrap();
    let (channels, _) = watch::channel(Vec::new());
    let app = crate::app::router(
        crate::app::AppState {
            status: crate::collector::status(&store),
            store,
            channels,
            port: 8794,
            configuration: Arc::new(tokio::sync::Mutex::new(())),
        },
        service.router(),
    );
    for (uri, expected) in [
        ("/api/community/status", StatusCode::OK),
        ("/api/community/settings", StatusCode::OK),
        ("/api/community/receipts?account=fixture", StatusCode::OK),
        ("/api/community/receipts?limit=101", StatusCode::BAD_REQUEST),
        ("/api/community/receipts?cursor=1", StatusCode::BAD_REQUEST),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("host", "127.0.0.1:8794")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{uri}");
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/community/login")
                .header("host", "127.0.0.1:8794")
                .header("origin", "https://untrusted.invalid")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[test]
fn source_projection_retains_signed_native_timing_and_distinct_source_identity() {
    let sample = source_sample();
    let CommunityCompletion::Success {
        provenance,
        members,
        ..
    } = completion(sample, store::now()).unwrap()
    else {
        panic!("success projection");
    };
    assert_eq!(members[0].roles, COMMUNITY_ROLE_VIEWER);
    assert_eq!(provenance.finished_minus_response_ms, Some(-50.0));
    assert_eq!(
        provenance.server_freshness,
        CommunityServerFreshness::Unknown
    );
}

pub(super) fn source_sample() -> SourceSample {
    SourceSample {
        received_at_unix_ms: 0,
        reported_chatters: Some(400),
        members: vec![community_source::host::Member {
            account: "alice".into(),
            roles: 32,
        }],
        category_counts: [0, 0, 0, 0, 0, 1],
        http_status: 200,
        provenance: community_source::sampling::Provenance {
            request_after_trigger_verified: true,
            request_after_trigger_ms: Some(10.0),
            request_to_response_ms: Some(100.0),
            request_to_finished_ms: Some(50.0),
            finished_minus_response_ms: Some(-50.0),
            from_disk_cache: Some(false),
            from_service_worker: Some(false),
            from_prefetch_cache: Some(false),
            encoded_bytes: Some(100),
            transport: "network_response",
            server_freshness: "unknown",
            ..Default::default()
        },
    }
}

pub(super) async fn pending_fixture() -> (tempfile::TempDir, Store, Coordinator) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("test.sqlite3"))
        .await
        .unwrap();
    let (_, mut runner) = Community::initialize(store.clone(), directory.path().join("profile"))
        .await
        .unwrap();
    runner.sync_channels(&["example".into()]);
    let run_id = store.begin_community_run("test".into()).await.unwrap();
    let sample_id = store
        .begin_community_sample(NewCommunitySample {
            run_id,
            job_id: 1,
            channel: "example".into(),
            started_at_utc: "1970-01-01T00:00:00.000Z".into(),
        })
        .await
        .unwrap();
    runner.run_id = Some(run_id);
    runner.pending = Some(Pending {
        job_id: 1,
        sample_id,
        channel: "example".into(),
        started: Instant::now(),
        cancelled: false,
        started_at_utc: "1970-01-01T00:00:00.000Z".into(),
    });
    (directory, store, runner)
}

fn worker_fault(contained: bool) -> FaultReport {
    FaultReport {
        worker: Some(WorkerDiagnostics {
            process_id: Some(123),
            started_at_unix_ms: Some(456),
            exit_code: contained.then_some(0xC000_0005),
            worker_tree_exited: contained,
            containment_confirmed: contained,
            failure: Some(community_source::host::WorkerFailure::UnexpectedExit),
        }),
        ..Default::default()
    }
}

fn browser_fault(contained: bool) -> FaultReport {
    let mut report = worker_fault(contained);
    report.worker.as_mut().unwrap().failure =
        Some(community_source::host::WorkerFailure::BrowserProcess);
    report.primary_failure = Some(primary_fixture("browser_process_failed", 1));
    report
}

fn cleanup_only_fault(contained: bool) -> FaultReport {
    let mut report = worker_fault(contained);
    let worker = report.worker.as_mut().unwrap();
    worker.failure = Some(community_source::host::WorkerFailure::CleanupUnconfirmed);
    worker.exit_code = contained.then_some(0);
    report.cleanup_diagnostics = Some(CleanupDiagnostics {
        controller_epoch: 7,
        expected_browser_pid: Some(100),
        close_hresult: Some(0),
        unsubscribe_hresult: Some(0),
        elapsed_ms: 5050,
        ..Default::default()
    });
    report.host_termination = Some(community_source::host::HostTermination {
        reason: community_source::host::HostTerminationReason::CleanupUnconfirmed,
        observed_at_unix_ms: 12,
        controller_epoch: 7,
        job_id: Some(1),
    });
    report
}

#[tokio::test]
async fn cleanup_only_retirement_recovers_after_exact_containment_and_preserves_success_and_fences()
{
    for delivery in [
        "status_first",
        "diagnostic_first",
        "terminal_only",
        "pending_failure",
    ] {
        let (_directory, store, mut runner) = pending_fixture().await;
        runner.settings.enabled = true;
        runner.next_start = Instant::now() + START_GAP;
        if delivery != "pending_failure" {
            runner
                .event(HostEvent::SampleCompleted {
                    job_id: 1,
                    channel: "example".into(),
                    outcome: Ok(source_sample()),
                })
                .await
                .unwrap();
        }
        if delivery == "status_first" {
            runner
                .pause_event("controller_cleanup_unconfirmed")
                .await
                .unwrap();
        }
        if delivery != "terminal_only" {
            runner
                .record_fault(cleanup_only_fault(false))
                .await
                .unwrap();
            if delivery == "diagnostic_first" {
                runner
                    .pause_event("controller_cleanup_unconfirmed")
                    .await
                    .unwrap();
            }
            assert!(runner.recovery_retry_at.is_none());
            assert_eq!(runner.quarantine_ticket, Some(runner.host_generation));
            assert_eq!(
                store.community_pause().await.unwrap(),
                Some(CommunityPauseReason::CleanupUnconfirmed)
            );
            runner.step().await.unwrap();
            assert_eq!(runner.host_start_attempts, 0);
        }
        if delivery == "pending_failure" {
            runner
                .event(HostEvent::SampleCompleted {
                    job_id: 1,
                    channel: "example".into(),
                    outcome: Err(community_source::host::SourceFailure {
                        code: "cleanup_unconfirmed",
                        pauses_community: true,
                    }),
                })
                .await
                .unwrap();
        }
        let global_fence = runner.next_start;
        let channel_fence = runner.schedule["example"].retry_not_before;
        tokio::time::pause();
        runner
            .event(HostEvent::Stopped {
                cleanup_confirmed: false,
                diagnostics: cleanup_only_fault(true),
            })
            .await
            .unwrap();
        assert!(!runner.paused && runner.replacement_safe);
        assert_eq!(
            runner.recovery_retry_at.unwrap() - Instant::now(),
            Duration::from_secs(5)
        );
        assert!(runner.pending.is_none() && runner.run_id.is_none());
        assert_eq!(runner.next_start, global_fence);
        assert_eq!(runner.schedule["example"].retry_not_before, channel_fence);
        runner.step().await.unwrap();
        assert_eq!(runner.host_start_attempts, 0);
        tokio::time::advance(Duration::from_secs(5)).await;
        runner.step().await.unwrap();
        assert_eq!(runner.host_start_attempts, 1);
        tokio::time::resume();
        assert_eq!(store.community_pause().await.unwrap(), None);
        let stats = store.community_stats().await.unwrap();
        assert_eq!(
            (
                stats.successful_count,
                stats.failed_count,
                stats.pending_count
            ),
            if delivery == "pending_failure" {
                (0, 1, 0)
            } else {
                (1, 0, 0)
            }
        );
        let reason: String = store
            .call_read(crate::diagnostics::Operation::Query, |conn| {
                Ok(conn.query_row(
                    "SELECT end_reason FROM community_runs WHERE id=1",
                    [],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(reason, "host_failure");
        let status = runner.service.status.read().await;
        assert_eq!(status.cleanup_confirmed, Some(false));
        assert!(status.primary_failure.is_none());
        let cleanup = status.cleanup_diagnostics.as_ref().unwrap();
        assert!(!cleanup.confirmed && !cleanup.browser_process_failed());
        assert!(cleanup.observed_exit_pid.is_none() && cleanup.observed_exit_kind.is_none());
    }
}

#[tokio::test]
async fn cleanup_only_retirement_never_clears_received_source_or_legacy_pause() {
    for code in [
        "authentication_denied",
        "integrity_denied",
        "challenge_denied",
        "rate_limited",
        "source_errors",
    ] {
        for order in ["before", "after"] {
            let (_directory, store, mut runner) = pending_fixture().await;
            runner.settings.enabled = true;
            if order == "before" {
                runner.pause_source(code).await.unwrap();
            }
            runner
                .record_fault(cleanup_only_fault(false))
                .await
                .unwrap();
            if order == "after" {
                runner
                    .record_fault(FaultReport {
                        source_barrier: Some(community_source::host::SourceBarrier { code }),
                        ..Default::default()
                    })
                    .await
                    .unwrap();
            }
            let barrier = store.community_pause().await.unwrap().unwrap();
            runner
                .event(HostEvent::Stopped {
                    cleanup_confirmed: false,
                    diagnostics: cleanup_only_fault(true),
                })
                .await
                .unwrap();
            assert!(
                runner.paused
                    && runner.recovery_retry_at.is_none()
                    && runner.quarantine_ticket.is_none()
            );
            assert_eq!(store.community_pause().await.unwrap(), Some(barrier));
            assert_ne!(barrier, CommunityPauseReason::CleanupUnconfirmed);
            assert_eq!(store.community_stats().await.unwrap().failed_count, 1);
        }
    }
    for legacy in [
        CommunityPauseReason::CleanupUnconfirmed,
        CommunityPauseReason::SourceDenied,
    ] {
        let (_directory, store, mut runner) = pending_fixture().await;
        runner.settings.enabled = true;
        runner.paused = true;
        runner.persisted_pause = Some(legacy);
        store.set_community_pause(Some(legacy)).await.unwrap();
        runner
            .record_fault(cleanup_only_fault(false))
            .await
            .unwrap();
        runner
            .pause_event("controller_cleanup_unconfirmed")
            .await
            .unwrap();
        runner
            .event(HostEvent::Stopped {
                cleanup_confirmed: false,
                diagnostics: cleanup_only_fault(true),
            })
            .await
            .unwrap();
        assert!(
            runner.paused
                && runner.recovery_retry_at.is_none()
                && runner.quarantine_ticket.is_none()
        );
        assert_eq!(store.community_pause().await.unwrap(), Some(legacy));
    }
}

#[tokio::test]
async fn owner_controls_cancel_cleanup_only_recovery_before_late_physical_proof() {
    for command in ["disable", "login", "shutdown"] {
        let (_directory, store, mut runner) = pending_fixture().await;
        runner.settings.enabled = true;
        runner
            .record_fault(cleanup_only_fault(false))
            .await
            .unwrap();
        match command {
            "disable" => {
                runner
                    .control(Command::Settings(CommunitySettingsUpdate {
                        enabled: false,
                        target_interval_seconds: 300,
                        cadence_mode: None,
                    }))
                    .await
                    .unwrap();
            }
            "login" => {
                runner.control(Command::Login).await.unwrap();
            }
            _ => {
                let (_, shutdown) = watch::channel(true);
                runner.shutdown = Some(shutdown);
                runner.step().await.unwrap();
            }
        }
        let starts = runner.host_start_attempts;
        runner
            .event(HostEvent::Stopped {
                cleanup_confirmed: false,
                diagnostics: cleanup_only_fault(true),
            })
            .await
            .unwrap();
        runner.step().await.unwrap();
        assert!(runner.quarantine_ticket.is_none() && runner.recovery_retry_at.is_none());
        assert_eq!(runner.host_start_attempts, starts);
        assert_eq!(
            store.community_pause().await.unwrap(),
            Some(CommunityPauseReason::CleanupUnconfirmed)
        );
    }
}

#[tokio::test]
async fn positive_receipt_then_a_known_failed_browser_exit_keeps_coverage_and_source_barriers_honest(
) {
    for source_denied in [false, true] {
        let (_directory, store, mut runner) = pending_fixture().await;
        runner.settings.enabled = true;
        runner
            .event(HostEvent::SampleCompleted {
                job_id: 1,
                channel: "example".into(),
                outcome: Ok(source_sample()),
            })
            .await
            .unwrap();
        if source_denied {
            runner.pause_source("source_denied").await.unwrap();
        }
        let fault = |contained| {
            let mut report = browser_fault(contained);
            report.primary_failure = None;
            report.cleanup_diagnostics = Some(CleanupDiagnostics {
                expected_browser_pid: Some(100),
                observed_exit_pid: Some(100),
                observed_exit_kind: Some(1),
                ..Default::default()
            });
            report
        };
        runner.record_fault(fault(false)).await.unwrap();
        assert!(runner.run_id.is_none());
        assert!(runner.recovery_retry_at.is_none());
        runner.step().await.unwrap();
        assert_eq!(runner.host_start_attempts, 0);
        runner
            .event(HostEvent::Stopped {
                cleanup_confirmed: false,
                diagnostics: fault(true),
            })
            .await
            .unwrap();
        assert_eq!(runner.recovery_retry_at.is_some(), !source_denied);
        assert_eq!(runner.paused, source_denied);
        let stats = store.community_stats().await.unwrap();
        assert_eq!(
            (
                stats.successful_count,
                stats.failed_count,
                stats.pending_count
            ),
            (1, 0, 0)
        );
        assert_eq!(
            store.community_pause().await.unwrap(),
            source_denied.then_some(CommunityPauseReason::SourceDenied)
        );
        let status = runner.service.status.read().await;
        assert!(status.primary_failure.is_none());
        assert_eq!(status.cleanup_confirmed, Some(false));
        assert!(!status.cleanup_diagnostics.as_ref().unwrap().confirmed);
    }
}

#[tokio::test]
async fn new_browser_subprocess_failure_waits_for_physical_proof_and_never_clears_an_existing_source_barrier(
) {
    for source in [None, Some("source_denied"), Some("authentication_denied")] {
        let (_directory, store, mut runner) = pending_fixture().await;
        runner.settings.enabled = true;
        if let Some(code) = source {
            runner.pause_source(code).await.unwrap();
        }
        runner.record_fault(browser_fault(false)).await.unwrap();
        runner
            .event(HostEvent::SampleCompleted {
                job_id: 1,
                channel: "example".into(),
                outcome: Err(community_source::host::SourceFailure {
                    code: "browser_process_failed",
                    pauses_community: false,
                }),
            })
            .await
            .unwrap();
        assert!(runner.recovery_retry_at.is_none());
        assert_eq!(runner.quarantine_ticket.is_some(), source.is_none());
        assert!(runner.run_id.is_none());
        let next_start = runner.next_start;
        let channel_retry = runner.schedule["example"].retry_not_before;
        runner
            .event(HostEvent::Stopped {
                cleanup_confirmed: false,
                diagnostics: browser_fault(true),
            })
            .await
            .unwrap();
        assert_eq!(runner.recovery_retry_at.is_some(), source.is_none());
        assert_eq!(runner.paused, source.is_some());
        assert_eq!(runner.next_start, next_start);
        assert_eq!(runner.schedule["example"].retry_not_before, channel_retry);
        assert_eq!(store.community_stats().await.unwrap().failed_count, 1);
        assert_eq!(
            store.community_pause().await.unwrap().is_some(),
            source.is_some()
        );
        if source.is_none() {
            runner.job_id = 1;
            runner.prepare_sample("example".into()).await.unwrap();
            let mut sample = source_sample();
            sample.received_at_unix_ms = Utc::now().timestamp_millis();
            runner
                .event(HostEvent::SampleCompleted {
                    job_id: 2,
                    channel: "example".into(),
                    outcome: Ok(sample),
                })
                .await
                .unwrap();
            assert_eq!(runner.worker_failures, 0);
            assert_eq!(store.community_stats().await.unwrap().successful_count, 1);
        }
    }
}

#[tokio::test]
async fn owner_controls_cancel_browser_failure_recovery_before_late_containment() {
    for command in ["disable", "login", "shutdown"] {
        let (_directory, store, mut runner) = pending_fixture().await;
        runner.settings.enabled = true;
        runner.record_fault(browser_fault(false)).await.unwrap();
        match command {
            "disable" => {
                runner
                    .control(Command::Settings(CommunitySettingsUpdate {
                        enabled: false,
                        target_interval_seconds: 300,
                        cadence_mode: None,
                    }))
                    .await
                    .unwrap();
            }
            "login" => {
                runner.control(Command::Login).await.unwrap();
            }
            _ => {
                let (_, shutdown) = watch::channel(true);
                runner.shutdown = Some(shutdown);
                runner.step().await.unwrap();
            }
        }
        let starts = runner.host_start_attempts;
        runner
            .event(HostEvent::Stopped {
                cleanup_confirmed: false,
                diagnostics: browser_fault(true),
            })
            .await
            .unwrap();
        runner.step().await.unwrap();
        assert!(runner.quarantine_ticket.is_none() && runner.recovery_retry_at.is_none());
        assert_eq!(runner.host_start_attempts, starts);
        assert_eq!(
            store.community_pause().await.unwrap(),
            Some(CommunityPauseReason::CleanupUnconfirmed)
        );
    }
}

async fn contained_terminal(runner: &mut Coordinator) {
    runner
        .event(HostEvent::Stopped {
            cleanup_confirmed: false,
            diagnostics: worker_fault(true),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn worker_death_while_pending_is_saved_keeps_recovery_intent_after_closed_dispatch() {
    for source_barrier in [false, true] {
        let (_directory, store, mut runner) = pending_fixture().await;
        runner.settings.enabled = true;
        if source_barrier {
            runner
                .record_fault(FaultReport {
                    source_barrier: Some(community_source::host::SourceBarrier {
                        code: "source_errors",
                    }),
                    ..Default::default()
                })
                .await
                .unwrap();
        }
        runner
            .failed_dispatch(Some(community_source::host::CommandError::Closed))
            .await
            .unwrap();
        assert_eq!(runner.worker_fault_pending, !source_barrier);
        assert!(runner.pending.is_none() && runner.run_id.is_none());
        contained_terminal(&mut runner).await;
        assert_eq!(runner.recovery_retry_at.is_some(), !source_barrier);
        assert_eq!(runner.paused, source_barrier);
        assert_eq!(store.community_stats().await.unwrap().failed_count, 1);
        assert_eq!(store.community_stats().await.unwrap().pending_count, 0);
    }
}

#[tokio::test]
async fn contained_worker_fault_fails_once_then_retries_without_resume_and_preserves_fences() {
    let (_directory, store, mut runner) = pending_fixture().await;
    runner.settings.enabled = true;
    runner.next_start = Instant::now() + START_GAP;
    runner
        .event(HostEvent::SampleCompleted {
            job_id: 1,
            channel: "example".into(),
            outcome: Err(community_source::host::SourceFailure {
                code: "worker_failed",
                pauses_community: true,
            }),
        })
        .await
        .unwrap();
    assert!(runner.paused && runner.worker_fault_pending);
    assert_eq!(store.community_pause().await.unwrap(), None);
    let retry_fence = runner.schedule["example"].retry_not_before;
    let global_fence = runner.next_start;
    // Freeze before arming recovery so real SQLite I/O cannot consume its
    // five-second deadline. Durable writes have no read-response timeout.
    tokio::time::pause();
    contained_terminal(&mut runner).await;
    runner.refresh_status().await;
    assert!(!runner.paused && runner.replacement_safe);
    assert!(runner.run_id.is_none() && runner.pending.is_none());
    {
        let status = runner.service.status.read().await;
        assert_eq!(status.state, "recovering");
        assert_eq!(status.cleanup_confirmed, Some(false));
        let recovery = status.worker_recovery.as_ref().unwrap();
        assert_eq!((recovery.state, recovery.attempts), ("backoff", 1));
        assert!(recovery.next_retry_at_utc.is_some());
        assert!(!recovery.cleanup_confirmed);
        assert_eq!(
            recovery.last_fault.worker.as_ref().unwrap().exit_code,
            Some(0xC000_0005)
        );
        assert!(recovery.last_fault.cleanup_diagnostics.is_none());
    }
    runner.step().await.unwrap();
    assert_eq!(runner.host_start_attempts, 0);
    tokio::time::advance(Duration::from_secs(5)).await;
    runner.step().await.unwrap();
    // The test host returns a synchronous no-owner error. It must retry later,
    // not spin or reopen a sample, and must retain both sampling fences.
    assert_eq!(runner.host_start_attempts, 1);
    runner.step().await.unwrap();
    assert_eq!(runner.host_start_attempts, 1);
    assert_eq!(runner.schedule["example"].retry_not_before, retry_fence);
    assert_eq!(runner.next_start, global_fence);
    tokio::time::resume();
    // Reads use the real clock while the SQLite OS thread answers; a paused
    // runtime could otherwise auto-advance their response timeout.
    assert_eq!(store.community_pause().await.unwrap(), None);
    let stats = store.community_stats().await.unwrap();
    assert_eq!((stats.failed_count, stats.pending_count), (1, 0));
    assert_eq!(store.community_stats().await.unwrap().sample_count, 1);
}

#[tokio::test]
async fn repeated_contained_crashes_retry_indefinitely_at_the_cap_and_ready_does_not_reset() {
    let (_directory, _store, mut runner) = pending_fixture().await;
    // Fixture initialization reads SQLite before virtual scheduling begins.
    tokio::time::pause();
    runner.settings.enabled = true;
    for (index, seconds) in [5, 10, 20, 40, 60, 60, 60, 60].into_iter().enumerate() {
        contained_terminal(&mut runner).await;
        assert_eq!(runner.worker_failures, index as u32 + 1);
        assert_eq!(
            runner.recovery_retry_at.unwrap() - Instant::now(),
            Duration::from_secs(seconds)
        );
        runner.event(HostEvent::Ready).await.unwrap();
        assert_eq!(runner.worker_failures, index as u32 + 1);
        assert!(!runner.paused);
    }
}

#[tokio::test]
async fn durable_success_alone_resets_the_worker_streak_and_a_later_crash_opens_a_gap() {
    let (_directory, store, mut runner) = pending_fixture().await;
    runner.settings.enabled = true;
    contained_terminal(&mut runner).await;
    runner.job_id = 1;
    assert!(runner.prepare_sample("example".into()).await.unwrap());
    let replacement_run = runner.run_id.unwrap();
    let mut sample = source_sample();
    sample.received_at_unix_ms = Utc::now().timestamp_millis();
    runner
        .event(HostEvent::SampleCompleted {
            job_id: 2,
            channel: "example".into(),
            outcome: Ok(sample),
        })
        .await
        .unwrap();
    assert_eq!(runner.worker_failures, 0);
    assert_eq!(
        runner
            .service
            .status
            .read()
            .await
            .worker_recovery
            .as_ref()
            .unwrap()
            .state,
        "recovered"
    );
    contained_terminal(&mut runner).await;
    assert_eq!(runner.worker_failures, 1);
    let stats = store.community_stats().await.unwrap();
    assert_eq!(
        (
            stats.failed_count,
            stats.successful_count,
            stats.pending_count
        ),
        (1, 1, 0)
    );
    let reason: String = store
        .call_read(crate::diagnostics::Operation::Query, move |conn| {
            Ok(conn.query_row(
                "SELECT end_reason FROM community_runs WHERE id=?1",
                [replacement_run],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(reason, "host_failure");
    assert!(runner.run_id.is_none());
}

#[tokio::test]
async fn invalid_or_failed_durable_completion_never_resets_worker_recovery() {
    for invalid in [false, true] {
        let (_directory, store, mut runner) = pending_fixture().await;
        runner.settings.enabled = true;
        contained_terminal(&mut runner).await;
        runner.job_id = 1;
        runner.prepare_sample("example".into()).await.unwrap();
        let mut sample = source_sample();
        sample.received_at_unix_ms = Utc::now().timestamp_millis();
        if invalid {
            sample.reported_chatters = Some(u64::MAX);
        } else {
            store.call_write(crate::diagnostics::Operation::CommunityAppend, |conn| {
                conn.execute_batch("CREATE TEMP TRIGGER reject_recovery_success BEFORE UPDATE OF outcome ON community_samples WHEN NEW.outcome='success' BEGIN SELECT RAISE(ABORT,'fixture success failure'); END;")?;
                Ok(())
            }).await.unwrap();
        }
        let result = runner
            .event(HostEvent::SampleCompleted {
                job_id: 2,
                channel: "example".into(),
                outcome: Ok(sample),
            })
            .await;
        assert_eq!(runner.worker_failures, 1);
        assert!(invalid || result.is_err());
        assert_ne!(
            runner
                .service
                .status
                .read()
                .await
                .worker_recovery
                .as_ref()
                .unwrap()
                .state,
            "recovered"
        );
    }
}

#[tokio::test]
async fn every_physical_proof_component_is_required_and_legacy_cleanup_is_never_inferred_recoverable(
) {
    for missing in 0..6 {
        let (directory, store, mut runner) = pending_fixture().await;
        runner.settings.enabled = true;
        let mut report = worker_fault(true);
        let worker = report.worker.as_mut().unwrap();
        match missing {
            0 => worker.worker_tree_exited = false,
            1 => worker.containment_confirmed = false,
            2 => worker.exit_code = None,
            3 => worker.process_id = None,
            4 => worker.started_at_unix_ms = None,
            _ => worker.failure = None,
        }
        runner
            .event(HostEvent::Stopped {
                cleanup_confirmed: false,
                diagnostics: report,
            })
            .await
            .unwrap();
        assert!(runner.paused && runner.recovery_retry_at.is_none());
        assert_eq!(
            store.community_pause().await.unwrap(),
            Some(CommunityPauseReason::CleanupUnconfirmed)
        );
        runner.step().await.unwrap();
        assert_eq!(runner.host_start_attempts, 0);
        let (_, restored) = Community::initialize(store, directory.path().join("profile"))
            .await
            .unwrap();
        assert!(restored.paused && restored.quarantine_ticket.is_none());
    }
}

#[tokio::test]
async fn quarantine_persists_its_own_barrier_and_only_matching_late_proof_can_clear_it() {
    let (directory, store, mut runner) = pending_fixture().await;
    runner.settings.enabled = true;
    runner.record_fault(worker_fault(false)).await.unwrap();
    assert_eq!(runner.quarantine_ticket, Some(runner.host_generation));
    assert_eq!(
        store.community_pause().await.unwrap(),
        Some(CommunityPauseReason::CleanupUnconfirmed)
    );
    assert_eq!(
        runner
            .service
            .status
            .read()
            .await
            .worker_recovery
            .as_ref()
            .unwrap()
            .state,
        "awaiting_cleanup"
    );
    runner
        .event(HostEvent::SampleCompleted {
            job_id: 1,
            channel: "example".into(),
            outcome: Err(community_source::host::SourceFailure {
                code: "worker_failed",
                pauses_community: true,
            }),
        })
        .await
        .unwrap();
    // The bounded Paused event was dropped; the completion alone ends coverage.
    assert!(runner.run_id.is_none());
    let (_, restored) = Community::initialize(store.clone(), directory.path().join("profile"))
        .await
        .unwrap();
    assert!(restored.paused && restored.quarantine_ticket.is_none());
    contained_terminal(&mut runner).await;
    assert!(!runner.paused && runner.recovery_retry_at.is_some());
    assert_eq!(store.community_pause().await.unwrap(), None);
    assert_eq!(store.community_stats().await.unwrap().failed_count, 1);
}

#[tokio::test]
async fn idle_quarantine_closes_coverage_without_any_status_event_or_completion() {
    let (_directory, store, mut runner) = pending_fixture().await;
    runner.settings.enabled = true;
    runner
        .event(HostEvent::SampleCompleted {
            job_id: 1,
            channel: "example".into(),
            outcome: Ok(source_sample()),
        })
        .await
        .unwrap();
    assert!(runner.pending.is_none() && runner.run_id.is_some());
    runner.record_fault(worker_fault(false)).await.unwrap();
    assert!(runner.run_id.is_none());
    assert_eq!(store.community_stats().await.unwrap().successful_count, 1);
    assert_eq!(store.community_stats().await.unwrap().failed_count, 0);
    contained_terminal(&mut runner).await;
    assert!(!runner.paused && runner.recovery_retry_at.is_some());
}

#[tokio::test]
async fn shutdown_during_terminal_storage_never_commits_recovery_or_clears_owned_cleanup() {
    for quarantine in [false, true] {
        let (_directory, store, mut runner) = pending_fixture().await;
        runner.settings.enabled = true;
        if quarantine {
            runner.record_fault(worker_fault(false)).await.unwrap();
        }
        let (stop, shutdown) = watch::channel(false);
        runner.shutdown = Some(shutdown);
        let (release, blocker) = block_worker(store.clone()).await;
        {
            let terminal = runner.event(HostEvent::Stopped {
                cleanup_confirmed: false,
                diagnostics: worker_fault(true),
            });
            tokio::pin!(terminal);
            assert!(futures_util::poll!(terminal.as_mut()).is_pending());
            stop.send_replace(true);
            release.send(()).unwrap();
            terminal.await.unwrap();
        }
        blocker.await.unwrap().unwrap();
        assert!(
            runner.paused
                && runner.recovery_retry_at.is_none()
                && runner.quarantine_ticket.is_none()
        );
        assert_eq!(
            store.community_pause().await.unwrap(),
            quarantine.then_some(CommunityPauseReason::CleanupUnconfirmed)
        );
        runner.step().await.unwrap();
        assert_eq!(runner.host_start_attempts, 0);
    }
}

#[tokio::test]
async fn proved_no_child_launch_failures_retry_but_ambiguous_start_failures_stay_blocked() {
    for (failure, retries) in [
        (community_source::host::WorkerFailure::NotCreated, true),
        (community_source::host::WorkerFailure::Start, false),
    ] {
        let (_directory, store, mut runner) = pending_fixture().await;
        runner.settings.enabled = true;
        runner
            .event(HostEvent::Stopped {
                cleanup_confirmed: false,
                diagnostics: FaultReport {
                    worker: Some(WorkerDiagnostics {
                        failure: Some(failure),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();
        assert_eq!(runner.recovery_retry_at.is_some(), retries);
        assert_eq!(runner.paused, !retries);
        assert_eq!(
            store.community_pause().await.unwrap(),
            (!retries).then_some(CommunityPauseReason::CleanupUnconfirmed)
        );
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(5)).await;
        runner.step().await.unwrap();
        assert_eq!(runner.host_start_attempts, usize::from(retries));
        tokio::time::resume();
    }
}

#[tokio::test]
async fn owner_disable_login_and_shutdown_cancel_backoff_and_quarantine_without_late_resurrection()
{
    for quarantined in [false, true] {
        for command in ["disable", "login", "shutdown"] {
            let (_directory, _store, mut runner) = pending_fixture().await;
            runner.settings.enabled = true;
            if quarantined {
                runner.record_fault(worker_fault(false)).await.unwrap();
            } else {
                contained_terminal(&mut runner).await;
            }
            match command {
                "disable" => {
                    runner
                        .control(Command::Settings(CommunitySettingsUpdate {
                            enabled: false,
                            target_interval_seconds: 300,
                            cadence_mode: None,
                        }))
                        .await
                        .unwrap();
                }
                "login" => {
                    runner.control(Command::Login).await.unwrap();
                }
                _ => {
                    let (_, shutdown) = watch::channel(true);
                    runner.shutdown = Some(shutdown);
                    runner.step().await.unwrap();
                }
            }
            let starts = runner.host_start_attempts;
            assert!(runner.quarantine_ticket.is_none() && runner.recovery_retry_at.is_none());
            if quarantined {
                contained_terminal(&mut runner).await;
            }
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(600)).await;
            runner.step().await.unwrap();
            assert_eq!(
                runner.host_start_attempts, starts,
                "{command}, quarantine={quarantined}"
            );
            assert!(runner.recovery_retry_at.is_none());
            tokio::time::resume();
        }
    }
}

#[tokio::test]
async fn parent_received_source_barriers_always_outrank_worker_recovery_in_every_order() {
    for code in [
        "authentication_denied",
        "signed_in_ui_lost",
        "integrity_denied",
        "challenge_denied",
        "rate_limited",
        "http_denied",
        "source_denied",
    ] {
        for arrival in [
            "diagnostic_first",
            "terminal_only",
            "quarantined",
            "after_contained",
        ] {
            let (_directory, store, mut runner) = pending_fixture().await;
            runner.settings.enabled = true;
            if arrival == "quarantined" {
                runner.record_fault(worker_fault(false)).await.unwrap();
            }
            if arrival == "after_contained" {
                contained_terminal(&mut runner).await;
            }
            let mut report = worker_fault(true);
            report.primary_failure = Some(primary_fixture(code, 1));
            if arrival == "diagnostic_first" || arrival == "quarantined" {
                runner.record_fault(report.clone()).await.unwrap();
                report.primary_failure = None;
            }
            runner
                .event(HostEvent::Stopped {
                    cleanup_confirmed: false,
                    diagnostics: report,
                })
                .await
                .unwrap();
            assert!(
                runner.paused
                    && runner.quarantine_ticket.is_none()
                    && runner.recovery_retry_at.is_none(),
                "{code} {arrival}"
            );
            assert!(store
                .community_pause()
                .await
                .unwrap()
                .is_some_and(|reason| reason != CommunityPauseReason::CleanupUnconfirmed));
            runner.step().await.unwrap();
            assert_eq!(runner.host_start_attempts, 0);
            assert_eq!(store.community_stats().await.unwrap().failed_count, 1);
        }
    }
}

#[tokio::test]
async fn durable_completion_is_source_separate_and_wrong_job_cannot_commit() {
    let (_directory, store, mut runner) = pending_fixture().await;
    runner
        .event(HostEvent::SampleCompleted {
            job_id: 2,
            channel: "example".into(),
            outcome: Ok(source_sample()),
        })
        .await
        .unwrap();
    assert!(runner.paused);
    assert_eq!(store.community_stats().await.unwrap().pending_count, 1);
    runner
        .retire(CommunityRunEndReason::HostFailure)
        .await
        .unwrap();
    assert_eq!(store.community_stats().await.unwrap().member_count, 0);
    assert_eq!(store.community_stats().await.unwrap().cancelled_count, 1);

    let (_directory, store, mut runner) = pending_fixture().await;
    runner
        .event(HostEvent::SampleCompleted {
            job_id: 1,
            channel: "example".into(),
            outcome: Ok(source_sample()),
        })
        .await
        .unwrap();
    assert!(runner.pending.is_none());
    let stats = store.community_stats().await.unwrap();
    assert_eq!(stats.successful_count, 1);
    assert_eq!(stats.member_count, 1);
    assert_eq!(
        store
            .call_read(crate::diagnostics::Operation::Query, |conn| {
                Ok(
                    conn.query_row("SELECT COUNT(*) FROM observations", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                )
            })
            .await
            .unwrap(),
        0
    );
    let page = store
        .community_receipts(CommunityReceiptQuery {
            account: Some("alice".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].source, "community");
    runner
        .retire(CommunityRunEndReason::Shutdown)
        .await
        .unwrap();
}

#[tokio::test]
async fn denied_source_pauses_and_cancelled_job_does_not_save_members() {
    let (_directory, store, mut runner) = pending_fixture().await;
    runner
        .event(HostEvent::SampleCompleted {
            job_id: 1,
            channel: "example".into(),
            outcome: Err(community_source::host::SourceFailure {
                code: "integrity_denied",
                pauses_community: true,
            }),
        })
        .await
        .unwrap();
    assert!(runner.paused);
    assert_eq!(store.community_stats().await.unwrap().failed_count, 1);
    assert_eq!(
        runner.service.status.read().await.last_error,
        Some("integrity_denied")
    );
    runner.retire(CommunityRunEndReason::Paused).await.unwrap();

    let (_directory, store, mut runner) = pending_fixture().await;
    runner.sync_channels(&[]);
    runner
        .event(HostEvent::SampleCompleted {
            job_id: 1,
            channel: "example".into(),
            outcome: Ok(source_sample()),
        })
        .await
        .unwrap();
    assert_eq!(store.community_stats().await.unwrap().cancelled_count, 1);
    assert_eq!(store.community_stats().await.unwrap().member_count, 0);
    runner
        .retire(CommunityRunEndReason::Shutdown)
        .await
        .unwrap();
}

#[tokio::test]
async fn invalid_source_data_and_backwards_receipt_clock_pause_only_community() {
    for backwards_clock in [false, true] {
        let (_directory, store, mut runner) = pending_fixture().await;
        let (_stop, shutdown) = watch::channel(false);
        runner.shutdown = Some(shutdown.clone());
        let mut sample = source_sample();
        if backwards_clock {
            sample.received_at_unix_ms = -1;
        } else {
            sample.reported_chatters = Some(u64::MAX);
        }
        runner
            .event(HostEvent::SampleCompleted {
                job_id: 1,
                channel: "example".into(),
                outcome: Ok(sample),
            })
            .await
            .unwrap();
        assert!(runner.paused);
        assert!(!*shutdown.borrow());
        assert_eq!(
            runner.service.status.read().await.last_error,
            Some("invalid_source_sample")
        );
        let stats = store.community_stats().await.unwrap();
        assert_eq!(
            (stats.pending_count, stats.failed_count, stats.member_count),
            (0, 1, 0)
        );
        runner.retire(CommunityRunEndReason::Paused).await.unwrap();
    }
}

async fn block_worker(
    store: Store,
) -> (
    std::sync::mpsc::Sender<()>,
    tokio::task::JoinHandle<Result<(), StoreError>>,
) {
    let (entered, ready) = oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let task = tokio::spawn(async move {
        store
            .call_write(crate::diagnostics::Operation::CommunityAppend, move |_| {
                let _ = entered.send(());
                wait.recv_timeout(Duration::from_secs(5))
                    .map_err(|_| StoreError::Unavailable)?;
                Ok(())
            })
            .await
    });
    ready.await.unwrap();
    (release, task)
}

#[tokio::test]
async fn shutdown_during_pending_write_cancels_it_before_native_dispatch() {
    let (_directory, store, mut runner) = pending_fixture().await;
    runner
        .finish_failure(CommunityFailureCode::HostFailure, true)
        .await
        .unwrap();
    runner.job_id = 1;
    let (stop, shutdown) = watch::channel(false);
    runner.shutdown = Some(shutdown);
    let (release, blocker) = block_worker(store.clone()).await;
    {
        let future = runner.prepare_sample("example".into());
        tokio::pin!(future);
        assert!(futures_util::poll!(future.as_mut()).is_pending());
        stop.send_replace(true);
        release.send(()).unwrap();
        assert!(!future.await.unwrap());
    }
    blocker.await.unwrap().unwrap();
    assert!(runner.pending.is_none());
    assert_eq!(runner.host_start_attempts, 0);
    let stats = store.community_stats().await.unwrap();
    assert_eq!(
        (
            stats.pending_count,
            stats.cancelled_count,
            stats.member_count
        ),
        (0, 2, 0)
    );
    runner
        .retire(CommunityRunEndReason::Shutdown)
        .await
        .unwrap();
}

#[tokio::test]
async fn shutdown_during_sign_in_retirement_does_not_spawn_a_new_host() {
    let (directory, store, mut runner) = pending_fixture().await;
    let (stop, shutdown) = watch::channel(false);
    runner.shutdown = Some(shutdown);
    let (release, blocker) = block_worker(store).await;
    {
        let future = runner.control(Command::Login);
        tokio::pin!(future);
        assert!(futures_util::poll!(future.as_mut()).is_pending());
        stop.send_replace(true);
        release.send(()).unwrap();
        future.await.unwrap();
    }
    blocker.await.unwrap().unwrap();
    assert_eq!(runner.host_start_attempts, 0);
    assert!(runner.host.is_none());
    assert!(!directory.path().join("profile").exists());
}

#[tokio::test]
async fn denied_pause_restores_on_startup_and_only_explicit_resume_clears_it() {
    let (directory, store, mut runner) = pending_fixture().await;
    runner
        .event(HostEvent::SampleCompleted {
            job_id: 1,
            channel: "example".into(),
            outcome: Err(community_source::host::SourceFailure {
                code: "rate_limited",
                pauses_community: true,
            }),
        })
        .await
        .unwrap();
    runner.retire(CommunityRunEndReason::Paused).await.unwrap();
    runner
        .control(Command::Settings(CommunitySettingsUpdate {
            enabled: true,
            target_interval_seconds: 30,
            cadence_mode: Some(CommunityCadenceMode::Fixed),
        }))
        .await
        .unwrap();
    assert!(runner.paused);
    let (service, mut restored) =
        Community::initialize(store.clone(), directory.path().join("profile"))
            .await
            .unwrap();
    assert!(service.status.read().await.paused);
    assert_eq!(service.status.read().await.last_error, Some("rate_limited"));
    restored.sync_channels(&["example".into()]);
    restored.step().await.unwrap();
    assert_eq!(restored.host_start_attempts, 0);
    restored.control(Command::Login).await.unwrap();
    assert_eq!(
        store.community_pause().await.unwrap(),
        Some(CommunityPauseReason::RateLimited)
    );
    restored.control(Command::Resume).await.unwrap();
    assert!(!restored.paused);
    assert_eq!(store.community_pause().await.unwrap(), None);
}

#[tokio::test]
async fn sign_in_navigation_failure_keeps_owner_pause_and_denial_until_explicit_recovery() {
    for denial in [None, Some(CommunityPauseReason::RateLimited)] {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("test.sqlite3"))
            .await
            .unwrap();
        store.set_community_pause(denial).await.unwrap();
        let (service, mut runner) =
            Community::initialize(store.clone(), directory.path().join("profile"))
                .await
                .unwrap();
        runner
            .control(Command::Settings(CommunitySettingsUpdate {
                enabled: true,
                target_interval_seconds: 300,
                cadence_mode: Some(CommunityCadenceMode::Adaptive),
            }))
            .await
            .unwrap();
        runner.sync_channels(&["example".into()]);
        runner.event(HostEvent::LoginShown).await.unwrap();
        runner.event(HostEvent::LoginFailed).await.unwrap();
        runner.refresh_status().await;
        {
            let status = service.status.read().await;
            assert!(status.paused);
            assert_eq!(status.state, "paused");
            assert_eq!(status.last_error, Some("sign_in_navigation_failed"));
            assert!(status.next_due_at_utc.is_none());
        }
        for _ in 0..3 {
            runner.step().await.unwrap();
        }
        assert_eq!(
            runner.host_start_attempts, 0,
            "owner failure must not retry collection or login"
        );
        assert_eq!(store.community_pause().await.unwrap(), denial);
        assert_eq!(store.community_stats().await.unwrap().run_count, 0);

        // A newly shown explicit sign-in clears only its transient error and
        // restores any persisted source-denial explanation.
        runner.event(HostEvent::LoginShown).await.unwrap();
        assert!(runner.paused);
        assert_eq!(service.status.read().await.state, "sign_in");
        assert_eq!(
            service.status.read().await.last_error,
            denial.map(|reason| reason.code())
        );
        assert_eq!(store.community_pause().await.unwrap(), denial);
        runner.event(HostEvent::LoginFailed).await.unwrap();
        runner.control(Command::Login).await.unwrap();
        assert_eq!(
            runner.host_start_attempts, 1,
            "explicit login retains normal host creation"
        );
        assert!(runner.paused);
        assert_eq!(store.community_pause().await.unwrap(), denial);
        runner.event(HostEvent::LoginFailed).await.unwrap();
        runner.control(Command::Resume).await.unwrap();
        assert!(!runner.paused);
        assert_eq!(service.status.read().await.last_error, None);
        assert_eq!(service.status.read().await.state, "starting");
        assert_eq!(store.community_pause().await.unwrap(), None);
        assert!(!directory.path().join("profile").exists());
    }
}

#[tokio::test]
async fn unsolicited_terminal_ends_pending_or_idle_run_once_and_keeps_owner_pause() {
    for completed in [false, true] {
        let (_directory, store, mut runner) = pending_fixture().await;
        let run_id = runner.run_id.unwrap();
        if completed {
            runner
                .event(HostEvent::SampleCompleted {
                    job_id: 1,
                    channel: "example".into(),
                    outcome: Ok(source_sample()),
                })
                .await
                .unwrap();
        }
        runner.pause_source("rate_limited").await.unwrap();
        let termination = HostTermination {
            reason: community_source::host::HostTerminationReason::MessagePumpQuit,
            observed_at_unix_ms: 1234,
            controller_epoch: 7,
            job_id: (!completed).then_some(1),
        };
        runner
            .event(HostEvent::Stopped {
                cleanup_confirmed: true,
                diagnostics: FaultReport {
                    host_termination: Some(termination.clone()),
                    ..Default::default()
                },
            })
            .await
            .unwrap();
        assert!(runner.run_id.is_none());
        assert!(runner.pending.is_none());
        assert!(runner.paused);
        assert_eq!(
            store.community_pause().await.unwrap(),
            Some(CommunityPauseReason::RateLimited)
        );
        let boundary: (String, String) = store
            .call_read(crate::diagnostics::Operation::Query, move |conn| {
                Ok(conn.query_row(
                    "SELECT ended_at_utc,end_reason FROM community_runs WHERE id=?1",
                    [run_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(boundary.1, "host_failure");
        runner
            .retire(CommunityRunEndReason::Disabled)
            .await
            .unwrap();
        let retained: (String, String) = store
            .call_read(crate::diagnostics::Operation::Query, move |conn| {
                Ok(conn.query_row(
                    "SELECT ended_at_utc,end_reason FROM community_runs WHERE id=?1",
                    [run_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(retained, boundary);
        let diagnostic = store.diagnostics();
        assert!(diagnostic.first_failure.is_none());
        assert_eq!(
            diagnostic.latest.unwrap().host_termination,
            Some(termination)
        );
        let stats = store.community_stats().await.unwrap();
        assert_eq!(stats.failed_count, if completed { 0 } else { 1 });
        assert_eq!(stats.successful_count, if completed { 1 } else { 0 });
    }
}

#[tokio::test]
async fn terminal_during_retirement_keeps_explicit_run_boundary_and_login_pause() {
    let (_directory, store, mut runner) = pending_fixture().await;
    let run_id = runner.run_id.unwrap();
    runner.paused = true;
    runner.service.status.write().await.state = "sign_in";
    runner
        .record_fault(FaultReport {
            window_dismissal: Some(WindowDismissal {
                count: 1,
                last_observed_at_unix_ms: 1234,
            }),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(runner.paused);
    assert_eq!(runner.service.status.read().await.state, "sign_in");
    runner.retiring = true;
    runner
        .event(HostEvent::Stopped {
            cleanup_confirmed: true,
            diagnostics: FaultReport::default(),
        })
        .await
        .unwrap();
    assert_eq!(runner.run_id, Some(run_id));
    runner.retire(CommunityRunEndReason::SignIn).await.unwrap();
    let reason: String = store
        .call_read(crate::diagnostics::Operation::Query, move |conn| {
            Ok(conn.query_row(
                "SELECT end_reason FROM community_runs WHERE id=?1",
                [run_id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(reason, "sign_in");
    assert!(runner.paused);
    runner
        .retire(CommunityRunEndReason::Shutdown)
        .await
        .unwrap();
}

#[tokio::test]
async fn cleanup_terminal_cannot_erase_primary_source_failure_diagnostics() {
    let (_directory, store, mut runner) = pending_fixture().await;
    let report = FaultReport {
        primary_failure: Some(PrimaryFailure {
            code: "navigation_ui_deadline",
            stage: "preparing",
            observed_at_unix_ms: 1,
            controller_epoch: 7,
            job_id: Some(1),
            navigation_ready: true,
            last_player_observation: Some(community_source::sampling::PlayerObservation {
                video_paused: Some(true),
                ..Default::default()
            }),
            script_pending: true,
            network_pending: false,
            response_pending: false,
            trigger_pending: false,
            navigation_failure: None,
        }),
        cleanup_diagnostics: Some(CleanupDiagnostics {
            controller_epoch: 7,
            expected_browser_pid: Some(100),
            close_hresult: Some(0),
            unsubscribe_hresult: Some(0),
            elapsed_ms: 5000,
            ..Default::default()
        }),
        ..Default::default()
    };
    runner.record_fault(report.clone()).await.unwrap();
    runner
        .event(HostEvent::SampleCompleted {
            job_id: 1,
            channel: "example".into(),
            outcome: Err(community_source::host::SourceFailure {
                code: "navigation_ui_deadline",
                pauses_community: false,
            }),
        })
        .await
        .unwrap();
    runner
        .event(HostEvent::Stopped {
            cleanup_confirmed: false,
            diagnostics: report,
        })
        .await
        .unwrap();
    let status = runner.service.status.read().await.clone();
    assert_eq!(status.last_error, Some("cleanup_unconfirmed"));
    assert!(status.paused);
    let primary = status.primary_failure.unwrap();
    assert_eq!(
        (primary.code, primary.stage),
        ("navigation_ui_deadline", "preparing")
    );
    assert!(primary.script_pending && primary.navigation_ready);
    assert_eq!(
        primary.last_player_observation.unwrap().video_paused,
        Some(true)
    );
    assert!(!status.cleanup_diagnostics.unwrap().confirmed);
    assert_eq!(store.community_stats().await.unwrap().failed_count, 1);
    assert_eq!(
        store.community_pause().await.unwrap(),
        Some(CommunityPauseReason::CleanupUnconfirmed)
    );
    runner.retire(CommunityRunEndReason::Paused).await.unwrap();
}

#[tokio::test]
async fn matching_cancelled_denial_still_persists_the_pause() {
    let (_directory, store, mut runner) = pending_fixture().await;
    runner.sync_channels(&[]);
    runner
        .event(HostEvent::SampleCompleted {
            job_id: 1,
            channel: "example".into(),
            outcome: Err(community_source::host::SourceFailure {
                code: "integrity_denied",
                pauses_community: true,
            }),
        })
        .await
        .unwrap();
    assert_eq!(
        store.community_pause().await.unwrap(),
        Some(CommunityPauseReason::IntegrityDenied)
    );
    assert_eq!(store.community_stats().await.unwrap().cancelled_count, 1);
    assert_eq!(store.community_stats().await.unwrap().member_count, 0);
    runner
        .retire(CommunityRunEndReason::Shutdown)
        .await
        .unwrap();
}

#[tokio::test]
async fn denial_latch_survives_a_later_terminal_write_failure() {
    let (directory, store, mut runner) = pending_fixture().await;
    store
        .set_community_settings(CommunitySettings {
            enabled: true,
            target_interval_seconds: 30,
            cadence_mode: CommunityCadenceMode::Fixed,
        })
        .await
        .unwrap();
    store.call_write(crate::diagnostics::Operation::CommunityAppend, |conn| {
        conn.execute_batch("CREATE TEMP TRIGGER reject_terminal_fixture BEFORE UPDATE OF outcome ON community_samples WHEN NEW.outcome='failed' BEGIN SELECT RAISE(ABORT,'fixture terminal failure'); END;")?;
        Ok(())
    }).await.unwrap();
    let result = runner
        .event(HostEvent::SampleCompleted {
            job_id: 1,
            channel: "example".into(),
            outcome: Err(community_source::host::SourceFailure {
                code: "authentication_denied",
                pauses_community: true,
            }),
        })
        .await;
    assert!(matches!(result, Err(StoreError::Sqlite(_))));
    assert_eq!(
        store.community_pause().await.unwrap(),
        Some(CommunityPauseReason::AuthenticationDenied)
    );
    let (restored, _) = Community::initialize(store.clone(), directory.path().join("profile"))
        .await
        .unwrap();
    let status = restored.status.read().await;
    assert!(status.paused && status.settings.enabled);
    assert_eq!(store.community_stats().await.unwrap().pending_count, 1);
    assert_eq!(store.community_stats().await.unwrap().member_count, 0);
}

#[tokio::test]
async fn idle_and_pending_uncertain_cleanup_have_a_distinct_durable_barrier() {
    for completed in [false, true] {
        let (directory, store, mut runner) = pending_fixture().await;
        if completed {
            runner
                .event(HostEvent::SampleCompleted {
                    job_id: 1,
                    channel: "example".into(),
                    outcome: Ok(source_sample()),
                })
                .await
                .unwrap();
        }
        runner
            .event(HostEvent::Stopped {
                cleanup_confirmed: false,
                diagnostics: FaultReport::default(),
            })
            .await
            .unwrap();
        runner.refresh_status().await;
        assert!(runner.pending.is_none() && runner.run_id.is_none());
        assert_eq!(
            store.community_pause().await.unwrap(),
            Some(CommunityPauseReason::CleanupUnconfirmed)
        );
        let stats = store.community_stats().await.unwrap();
        assert_eq!(stats.failed_count, if completed { 0 } else { 1 });
        assert_eq!(stats.successful_count, if completed { 1 } else { 0 });
        let status = runner.service.status.read().await.clone();
        assert!(status.paused && status.next_due_at_utc.is_none());
        assert_eq!(status.last_error, Some("cleanup_unconfirmed"));
        assert_eq!(
            status.persisted_pause_reason,
            Some(CommunityPauseReason::CleanupUnconfirmed)
        );
        assert_eq!(status.cleanup_confirmed, Some(false));
        assert!(status.primary_failure.is_none());

        for enabled in [false, true] {
            runner
                .control(Command::Settings(CommunitySettingsUpdate {
                    enabled,
                    target_interval_seconds: 30,
                    cadence_mode: Some(CommunityCadenceMode::Fixed),
                }))
                .await
                .unwrap();
        }
        runner.control(Command::Login).await.unwrap();
        runner.control(Command::Resume).await.unwrap();
        runner.step().await.unwrap();
        assert_eq!(runner.host_start_attempts, 0);
        assert!(runner.paused);
        assert_eq!(
            store.community_pause().await.unwrap(),
            Some(CommunityPauseReason::CleanupUnconfirmed)
        );

        let (restored_service, mut restored) =
            Community::initialize(store.clone(), directory.path().join("profile"))
                .await
                .unwrap();
        restored.sync_channels(&["example".into()]);
        restored.step().await.unwrap();
        let status = restored_service.status.read().await.clone();
        assert!(status.paused);
        assert_eq!(
            status.persisted_pause_reason,
            Some(CommunityPauseReason::CleanupUnconfirmed)
        );
        assert_eq!(status.last_error, Some("cleanup_unconfirmed"));
        assert_eq!(
            status.cleanup_confirmed, None,
            "restart must not invent current-process cleanup telemetry"
        );
        assert!(status.primary_failure.is_none() && status.host_termination.is_none());
        assert_eq!(restored.host_start_attempts, 0);
        restored.control(Command::Login).await.unwrap();
        assert_eq!(
            store.community_pause().await.unwrap(),
            Some(CommunityPauseReason::CleanupUnconfirmed)
        );
        restored.control(Command::Resume).await.unwrap();
        assert!(!restored.paused);
        assert_eq!(store.community_pause().await.unwrap(), None);
        assert_eq!(
            restored_service.status.read().await.persisted_pause_reason,
            None
        );
        assert!(!directory.path().join("profile").exists());
    }
}

#[tokio::test]
async fn cleanup_codes_and_terminal_cancellation_are_never_source_denials() {
    for code in [
        "cleanup_unconfirmed",
        "controller_cleanup_unconfirmed",
        "operator_cancelled",
    ] {
        let (_directory, store, mut runner) = pending_fixture().await;
        runner
            .event(HostEvent::SampleCompleted {
                job_id: 1,
                channel: "example".into(),
                outcome: Err(community_source::host::SourceFailure {
                    code,
                    pauses_community: true,
                }),
            })
            .await
            .unwrap();
        if code != "operator_cancelled" {
            // A live cleanup status carries infrastructure intent, but cannot
            // grant recovery without this parent's exact containment evidence.
            assert_eq!(store.community_pause().await.unwrap(), None);
            runner
                .event(HostEvent::Stopped {
                    cleanup_confirmed: false,
                    diagnostics: FaultReport::default(),
                })
                .await
                .unwrap();
        }
        assert_eq!(
            store.community_pause().await.unwrap(),
            Some(CommunityPauseReason::CleanupUnconfirmed),
            "{code}"
        );
        assert_eq!(
            runner.service.status.read().await.last_error,
            Some("cleanup_unconfirmed")
        );
        assert_eq!(
            runner.service.status.read().await.cleanup_confirmed,
            Some(false)
        );
        runner.step().await.unwrap();
        assert_eq!(runner.host_start_attempts, 0);
    }

    let (_directory, store, mut runner) = pending_fixture().await;
    runner
        .event(HostEvent::SampleCompleted {
            job_id: 1,
            channel: "example".into(),
            outcome: Err(community_source::host::SourceFailure {
                code: "operator_cancelled",
                pauses_community: false,
            }),
        })
        .await
        .unwrap();
    assert_eq!(store.community_pause().await.unwrap(), None);
    assert!(!runner.paused);
    assert_eq!(store.community_stats().await.unwrap().cancelled_count, 1);
}

#[tokio::test]
async fn every_source_barrier_precedes_later_cleanup_and_retains_first_source_evidence() {
    for (code, reason) in [
        (
            "authentication_denied",
            CommunityPauseReason::AuthenticationDenied,
        ),
        ("integrity_denied", CommunityPauseReason::IntegrityDenied),
        ("challenge_denied", CommunityPauseReason::ChallengeDenied),
        ("rate_limited", CommunityPauseReason::RateLimited),
        ("source_denied", CommunityPauseReason::SourceDenied),
    ] {
        let (_directory, store, mut runner) = pending_fixture().await;
        runner
            .record_fault(FaultReport {
                primary_failure: Some(primary_fixture(code, 1)),
                ..Default::default()
            })
            .await
            .unwrap();
        runner.pause_source(code).await.unwrap();
        runner
            .event(HostEvent::SampleCompleted {
                job_id: 1,
                channel: "example".into(),
                outcome: Err(community_source::host::SourceFailure {
                    code: "operator_cancelled",
                    pauses_community: true,
                }),
            })
            .await
            .unwrap();
        runner
            .event(HostEvent::Stopped {
                cleanup_confirmed: false,
                diagnostics: FaultReport {
                    primary_failure: Some(primary_fixture("controller_cleanup_unconfirmed", 2)),
                    cleanup_diagnostics: Some(CleanupDiagnostics {
                        confirmed: false,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();
        assert_eq!(
            store.community_pause().await.unwrap(),
            Some(reason),
            "{code}"
        );
        let status = runner.service.status.read().await;
        assert_eq!(status.persisted_pause_reason, Some(reason));
        assert_eq!(status.last_error, Some(code));
        assert_eq!(status.primary_failure.as_ref().unwrap().code, code);
        assert_eq!(
            status.primary_failure.as_ref().unwrap().observed_at_unix_ms,
            1
        );
        assert_eq!(status.cleanup_confirmed, Some(false));
        assert!(!status.cleanup_diagnostics.as_ref().unwrap().confirmed);
    }
}

fn primary_fixture(code: &'static str, observed_at_unix_ms: i64) -> PrimaryFailure {
    PrimaryFailure {
        code,
        stage: "preparing",
        observed_at_unix_ms,
        controller_epoch: 7,
        job_id: Some(1),
        navigation_ready: true,
        last_player_observation: None,
        script_pending: false,
        network_pending: false,
        response_pending: false,
        trigger_pending: false,
        navigation_failure: None,
    }
}

#[tokio::test]
async fn retirement_timeout_persists_before_completion_for_every_control_boundary() {
    for reason in [
        CommunityRunEndReason::Shutdown,
        CommunityRunEndReason::Disabled,
        CommunityRunEndReason::SignIn,
        CommunityRunEndReason::Paused,
        CommunityRunEndReason::HostFailure,
    ] {
        let (directory, store, mut runner) = pending_fixture().await;
        runner.settings.enabled = true;
        store.set_community_settings(runner.settings).await.unwrap();
        runner.retiring = true;
        runner.pending.as_mut().unwrap().cancelled = true;
        // Exercise the exact fallback called after the real retirement deadline
        // with a host still present; unit fixtures never construct a native host.
        runner.finish_retirement(reason, true).await.unwrap();
        assert!(
            runner.paused && !runner.ready && runner.pending.is_none() && runner.run_id.is_none()
        );
        assert_eq!(
            store.community_pause().await.unwrap(),
            Some(CommunityPauseReason::CleanupUnconfirmed)
        );
        assert_eq!(store.community_stats().await.unwrap().cancelled_count, 1);
        let (restored, mut coordinator) =
            Community::initialize(store.clone(), directory.path().join("profile"))
                .await
                .unwrap();
        coordinator.sync_channels(&["example".into()]);
        coordinator.step().await.unwrap();
        assert!(restored.status.read().await.paused);
        assert_eq!(coordinator.host_start_attempts, 0);
    }
}

#[tokio::test]
async fn cleanup_barrier_precedes_terminal_write_failure_and_storage_failure_closes_admission() {
    for retirement_timeout in [false, true] {
        let (_directory, store, mut runner) = pending_fixture().await;
        store.call_write(crate::diagnostics::Operation::CommunityAppend, |conn| {
            conn.execute_batch("CREATE TEMP TRIGGER reject_cleanup_terminal BEFORE UPDATE OF outcome ON community_samples BEGIN SELECT RAISE(ABORT,'fixture terminal failure'); END;")?;
            Ok(())
        }).await.unwrap();
        let result = if retirement_timeout {
            runner
                .finish_retirement(CommunityRunEndReason::Paused, true)
                .await
        } else {
            runner
                .event(HostEvent::Stopped {
                    cleanup_confirmed: false,
                    diagnostics: FaultReport::default(),
                })
                .await
        };
        assert!(result.is_err());
        assert!(runner.paused);
        assert_eq!(
            store.community_pause().await.unwrap(),
            Some(CommunityPauseReason::CleanupUnconfirmed)
        );
        assert_eq!(store.community_stats().await.unwrap().pending_count, 1);
    }

    let (_directory, store, mut runner) = pending_fixture().await;
    runner.settings.enabled = true;
    store.set_community_settings(runner.settings).await.unwrap();
    store.call_write(crate::diagnostics::Operation::CommunityAppend, |conn| {
        conn.execute_batch("CREATE TEMP TRIGGER reject_cleanup_pause BEFORE UPDATE ON community_pause BEGIN SELECT RAISE(ABORT,'fixture pause failure'); END;")?;
        Ok(())
    }).await.unwrap();
    assert!(runner
        .finish_retirement(CommunityRunEndReason::Paused, true)
        .await
        .is_err());
    assert!(runner.paused);
    assert_eq!(
        runner.service.status.read().await.cleanup_confirmed,
        Some(false)
    );
    assert_eq!(
        runner.service.status.read().await.persisted_pause_reason,
        None,
        "failed storage must not claim a durable barrier"
    );
    assert_eq!(store.community_pause().await.unwrap(), None);
    assert_eq!(store.community_stats().await.unwrap().pending_count, 1);
    runner.step().await.unwrap();
    assert_eq!(runner.host_start_attempts, 0);
}

#[tokio::test]
async fn confirmed_retirement_and_recovered_sample_keep_raw_diagnostics_without_a_cleanup_barrier()
{
    let (_directory, store, mut runner) = pending_fixture().await;
    runner
        .record_fault(FaultReport {
            primary_failure: Some(primary_fixture("verified_player_pause_lost", 1)),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut recovered = source_sample();
    recovered.received_at_unix_ms = 2;
    runner
        .event(HostEvent::SampleCompleted {
            job_id: 1,
            channel: "example".into(),
            outcome: Ok(recovered),
        })
        .await
        .unwrap();
    let status = runner.service.status.read().await.clone();
    assert_eq!(status.last_error, None);
    assert_eq!(
        status.primary_failure.unwrap().code,
        "verified_player_pause_lost"
    );
    runner
        .retire(CommunityRunEndReason::Shutdown)
        .await
        .unwrap();
    assert_eq!(store.community_pause().await.unwrap(), None);
    assert!(!runner.paused);
}
