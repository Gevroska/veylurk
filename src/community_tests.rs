use super::*;
use axum::{body::Body, http::Request};
use tower::ServiceExt;

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
        .await;
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
    runner.record_fault(report.clone()).await;
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
        Some(CommunityPauseReason::SourceDenied)
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
