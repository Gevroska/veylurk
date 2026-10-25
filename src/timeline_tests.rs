use super::*;
use crate::store::NewObservation;

fn at(time: &str) -> String {
    format!("2026-01-01T{time}.000Z")
}

fn event(account: &str, kind: &str, time: &str) -> NewObservation {
    NewObservation {
        channel: "example".into(),
        account: Some(account.into()),
        kind: kind.into(),
        received_at: at(time),
    }
}

async fn fixture() -> (tempfile::TempDir, Store, i64) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("timeline.sqlite3"))
        .await
        .unwrap();
    crate::metadata::initialize(store.clone()).await.unwrap();
    store.track(vec!["example".into()], true).await.unwrap();
    let session = store
        .begin_session_at(vec!["example".into()], at("00:00:00"))
        .await
        .unwrap();
    (dir, store, session)
}

fn query() -> TimelineQuery {
    TimelineQuery {
        account: Some("alice".into()),
        from: Some(at("00:00:00")),
        to: Some(at("01:00:00")),
        ..TimelineQuery::default()
    }
}

async fn timeline(store: &Store, query: TimelineQuery) -> Timeline {
    store.timeline_at(query, at("01:00:00")).await.unwrap()
}

async fn broadcast(store: &Store) {
    store.call_write(Operation::MetadataInitialize, |conn| {
        conn.execute("INSERT INTO metadata_broadcasts(id,stream_id,channel,title,game_name,language,started_at,first_seen_at,last_live_at,offline_observed_at) VALUES(7,'7','example','Fixture','','en',?1,?1,?2,?3)",params![at("00:02:00"),at("00:05:00"),at("00:59:00")])?;
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn prior_connected_interval_is_clipped_to_broadcast_without_fabricating_positive_points() {
    let (_dir, store, session) = fixture().await;
    store
        .append(
            session,
            vec![
                event("alice", "join", "00:01:00"),
                event("alice", "part", "00:04:00"),
            ],
        )
        .await
        .unwrap();
    broadcast(&store).await;
    let page = timeline(
        &store,
        TimelineQuery {
            broadcast_id: Some(7),
            ..query()
        },
    )
    .await;
    assert_eq!(page.effective_range.from, at("00:02:00"));
    assert_eq!(page.effective_range.to, "2026-01-01T00:05:00.001Z");
    let visit = &page.intervals[0];
    assert_eq!(visit.first_observed_at, Some(at("00:01:00")));
    assert_eq!(visit.inference_from, Some(at("00:02:00")));
    assert_eq!(visit.inference_to, Some(at("00:04:00")));
    assert_eq!(visit.inference_end_basis.as_deref(), Some("part"));
    assert_eq!(visit.boundary_basis.as_deref(), Some("part"));
    assert_eq!(visit.markers.len(), 1);
    assert_eq!(visit.markers[0].kind, "part");
    assert_eq!(visit.markers[0].at, at("00:04:00"));
    assert_eq!(page.confidence, None);
    assert_eq!(page.confidence_status, "not_calibrated");
}

#[tokio::test]
async fn clipped_first_and_last_receipts_do_not_become_synthetic_markers() {
    let (_dir, store, session) = fixture().await;
    store
        .append(
            session,
            vec![
                event("alice", "join", "00:01:00"),
                event("alice", "names", "00:09:00"),
            ],
        )
        .await
        .unwrap();
    let page = timeline(
        &store,
        TimelineQuery {
            from: Some(at("00:02:00")),
            to: Some(at("00:08:00")),
            ..query()
        },
    )
    .await;
    let visit = &page.intervals[0];
    assert!(visit.markers.is_empty());
    assert_eq!(visit.inference_from, Some(at("00:02:00")));
    assert_eq!(visit.inference_to, Some(at("00:08:00")));
    assert_eq!(visit.inference_end_basis.as_deref(), Some("range_end"));
}

#[tokio::test]
async fn standalone_and_attached_parts_are_preserved_once_without_positive_or_absence_claims() {
    let (_dir, store, session) = fixture().await;
    store
        .append(
            session,
            vec![
                event("alice", "part", "00:01:00"),
                event("alice", "join", "00:02:00"),
                event("alice", "part", "00:03:00"),
                event("alice", "part", "00:04:00"),
            ],
        )
        .await
        .unwrap();
    let page = timeline(&store, query()).await;
    assert_eq!(page.intervals.len(), 3);
    let departures = page
        .intervals
        .iter()
        .filter(|row| row.kind == "departure")
        .collect::<Vec<_>>();
    assert_eq!(departures.len(), 2);
    assert!(departures.iter().all(|row| row.first_observed_at.is_none()
        && row.last_observed_at.is_none()
        && row.inference_from.is_none()
        && !row.supported_visit));
    assert_eq!(
        page.intervals
            .iter()
            .flat_map(|row| &row.markers)
            .filter(|marker| marker.kind == "part")
            .count(),
        3
    );
    let visit = page
        .intervals
        .iter()
        .find(|row| row.kind == "visit")
        .unwrap();
    assert_eq!(visit.last_observed_at, Some(at("00:02:00")));
    assert_eq!(visit.inference_to, Some(at("00:03:00")));
    let markers = page
        .intervals
        .iter()
        .flat_map(|row| &row.markers)
        .collect::<Vec<_>>();
    assert_eq!(
        markers
            .iter()
            .filter(|marker| marker.kind == "first_positive")
            .count(),
        1
    );
    assert_eq!(
        markers
            .iter()
            .filter(|marker| marker.kind == "last_positive")
            .count(),
        0
    );
}

#[tokio::test]
async fn gaps_retries_and_transport_ends_split_inference() {
    let (_dir, store, session) = fixture().await;
    store
        .append(session, vec![event("alice", "join", "00:01:00")])
        .await
        .unwrap();
    store
        .call_write(Operation::RoomState, move |conn| {
            crate::history::close_room(
                conn,
                session,
                "example",
                "coverage_gap",
                "membership_unavailable",
                &at("00:03:00"),
            )?;
            Ok(())
        })
        .await
        .unwrap();
    store
        .append(session, vec![event("alice", "names", "00:04:00")])
        .await
        .unwrap();
    store
        .end_session_at(session, "transport_disconnected".into(), at("00:06:00"))
        .await
        .unwrap();
    let retry = store
        .begin_session_at(vec!["example".into()], at("00:08:00"))
        .await
        .unwrap();
    store
        .append(retry, vec![event("alice", "join", "00:09:00")])
        .await
        .unwrap();
    store
        .end_session_at(retry, "shutdown".into(), at("00:10:00"))
        .await
        .unwrap();
    let page = timeline(&store, query()).await;
    let spans = page
        .intervals
        .iter()
        .map(|row| {
            (
                row.inference_from.clone().unwrap(),
                row.inference_to.clone().unwrap(),
                row.boundary_basis.clone().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        spans,
        vec![
            (at("00:09:00"), at("00:10:00"), "session_end".into()),
            (at("00:04:00"), at("00:06:00"), "session_end".into()),
            (at("00:01:00"), at("00:03:00"), "coverage_gap".into())
        ]
    );
    assert_eq!(
        page.intervals
            .iter()
            .flat_map(|row| &row.markers)
            .filter(|marker| marker.kind == "part")
            .count(),
        0
    );
}

#[tokio::test]
async fn restart_uses_last_retained_receipt_instead_of_recovery_persistence_time() {
    let (dir, store, session) = fixture().await;
    store
        .append(
            session,
            vec![
                event("alice", "join", "00:01:00"),
                event("other", "join", "00:03:00"),
            ],
        )
        .await
        .unwrap();
    let path = dir.path().join("timeline.sqlite3");
    drop(store);
    let mut reopened = None;
    for _ in 0..50 {
        match Store::open(&path).await {
            Ok(store) => {
                reopened = Some(store);
                break;
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
        }
    }
    let store = reopened.expect("isolated storage lock released after worker shutdown");
    let page = timeline(&store, query()).await;
    let visit = &page.intervals[0];
    assert!(visit.session_end_uncertain);
    assert_eq!(visit.end_reason.as_deref(), Some("process_interrupted"));
    assert_eq!(visit.boundary_at, Some(at("00:03:00")));
    assert_eq!(visit.inference_to, Some(at("00:03:00")));
    assert_eq!(visit.markers.len(), 1);
}

#[tokio::test]
async fn delayed_rosters_and_backwards_clocks_have_points_without_inference() {
    let (_dir, store, session) = fixture().await;
    store
        .append(
            session,
            vec![
                event("alice", "join", "00:01:00"),
                event("alice", "part", "00:02:00"),
                event("alice", "names", "00:03:00"),
                event("alice", "names", "00:02:30"),
            ],
        )
        .await
        .unwrap();
    let page = timeline(&store, query()).await;
    assert_eq!(page.intervals.len(), 3);
    for visit in &page.intervals[..2] {
        assert_eq!(visit.start_basis, "uncertain_names");
        assert!(!visit.supported_visit);
        assert!(visit.inference_from.is_none());
        assert!(!visit.markers.is_empty());
    }
    assert!(page.intervals[0].out_of_order);
    assert!(page.intervals[1].out_of_order);
    store
        .append(
            session,
            vec![
                event("bob", "join", "00:10:00"),
                event("bob", "part", "00:09:00"),
            ],
        )
        .await
        .unwrap();
    let page = timeline(
        &store,
        TimelineQuery {
            account: Some("bob".into()),
            ..query()
        },
    )
    .await;
    assert!(page.intervals[0].out_of_order);
    assert!(page.intervals[0].inference_from.is_none());
    assert_eq!(
        page.intervals[0]
            .markers
            .iter()
            .filter(|marker| marker.kind == "part")
            .count(),
        1
    );
}

#[tokio::test]
async fn inconsistent_stored_boundaries_are_points_only() {
    let (_dir, store, session) = fixture().await;
    store
        .append(
            session,
            vec![
                event("alice", "join", "00:01:00"),
                event("alice", "names", "00:03:00"),
            ],
        )
        .await
        .unwrap();
    store
        .call_write(Operation::Initialize, |conn| {
            conn.execute(
                "UPDATE history_visits SET closed_at=?1,end_basis='coverage_gap'",
                [at("00:02:00")],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let page = timeline(&store, query()).await;
    assert!(page.intervals[0].inference_from.is_none());
    assert_eq!(page.intervals[0].markers.len(), 2);
}

#[tokio::test]
async fn inconsistent_part_cutoff_cannot_extend_inference_beyond_its_actual_receipt() {
    let (_dir, store, session) = fixture().await;
    store
        .append(
            session,
            vec![
                event("alice", "join", "00:01:00"),
                event("alice", "part", "00:03:00"),
            ],
        )
        .await
        .unwrap();
    store
        .call_write(Operation::Initialize, |conn| {
            conn.execute("UPDATE history_visits SET closed_at=?1", [at("00:04:00")])?;
            Ok(())
        })
        .await
        .unwrap();
    let page = timeline(&store, query()).await;
    assert!(page.intervals[0].inference_from.is_none());
    assert_eq!(page.intervals[0].boundary_at, Some(at("00:03:00")));
    assert_eq!(page.intervals[0].boundary_basis.as_deref(), Some("part"));
    assert_eq!(
        page.intervals[0]
            .markers
            .iter()
            .find(|marker| marker.kind == "part")
            .unwrap()
            .at,
        at("00:03:00")
    );
}

#[tokio::test]
async fn markers_and_intervals_use_exact_half_open_boundaries() {
    let (_dir, store, session) = fixture().await;
    store
        .append(
            session,
            vec![
                event("alice", "join", "00:01:00"),
                event("alice", "part", "00:02:00"),
            ],
        )
        .await
        .unwrap();
    let page = timeline(
        &store,
        TimelineQuery {
            from: Some(at("00:01:00")),
            to: Some(at("00:02:00")),
            ..query()
        },
    )
    .await;
    assert_eq!(page.intervals[0].markers.len(), 1);
    assert_eq!(page.intervals[0].markers[0].kind, "first_positive");
    let page = timeline(
        &store,
        TimelineQuery {
            from: Some(at("00:02:00")),
            to: Some(at("00:03:00")),
            ..query()
        },
    )
    .await;
    assert!(page.intervals[0].inference_from.is_none());
    assert_eq!(page.intervals[0].markers.len(), 1);
    assert_eq!(page.intervals[0].markers[0].kind, "part");
    let page = timeline(
        &store,
        TimelineQuery {
            from: Some("2026-01-01T00:02:00.001Z".into()),
            to: Some(at("00:03:00")),
            ..query()
        },
    )
    .await;
    assert!(page.intervals.is_empty());
}

#[tokio::test]
async fn disjoint_broadcast_range_returns_ordered_empty_window_and_ignores_offline_delay() {
    let (_dir, store, session) = fixture().await;
    store
        .append(session, vec![event("alice", "join", "00:01:00")])
        .await
        .unwrap();
    broadcast(&store).await;
    for (from, to) in [("00:00:00", "00:01:00"), ("00:06:00", "00:07:00")] {
        let page = timeline(
            &store,
            TimelineQuery {
                broadcast_id: Some(7),
                from: Some(at(from)),
                to: Some(at(to)),
                ..query()
            },
        )
        .await;
        assert!(page.empty);
        assert_eq!(page.effective_range.from, page.effective_range.to);
        assert!(page.intervals.is_empty());
        assert!(!page.has_more);
    }
    let page = timeline(
        &store,
        TimelineQuery {
            broadcast_id: Some(7),
            ..query()
        },
    )
    .await;
    assert_eq!(
        page.intervals[0].inference_to.as_deref(),
        Some("2026-01-01T00:05:00.001Z")
    );
}

#[tokio::test]
async fn keyset_merges_standalone_departures_and_visits_and_preserves_axis() {
    let (_dir, store, session) = fixture().await;
    store
        .append(
            session,
            vec![
                event("alice", "part", "00:01:00"),
                event("alice", "join", "00:02:00"),
                event("alice", "part", "00:03:00"),
                event("alice", "part", "00:04:00"),
                event("alice", "join", "00:05:00"),
            ],
        )
        .await
        .unwrap();
    let first = timeline(
        &store,
        TimelineQuery {
            limit: Some(2),
            ..query()
        },
    )
    .await;
    assert_eq!(
        first.intervals.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![5, 4]
    );
    assert!(first.has_more);
    assert_eq!(first.next_before, Some(4));
    let next = store
        .timeline_at(
            TimelineQuery {
                before: first.next_before,
                limit: Some(2),
                from: Some(first.effective_range.from.clone()),
                to: Some(first.effective_range.to.clone()),
                ..query()
            },
            at("02:00:00"),
        )
        .await
        .unwrap();
    assert_eq!(
        next.intervals.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![2, 1]
    );
    assert!(!next.has_more);
    assert_eq!(next.next_before, None);
    assert_eq!(first.effective_range.from, next.effective_range.from);
    assert_eq!(first.effective_range.to, next.effective_range.to);
    assert_eq!(first.as_of, next.as_of);
    assert_ne!(first.generated_at, next.generated_at);
}

#[tokio::test]
async fn validation_bounds_defaults_and_removed_channel_history_are_retained() {
    let (_dir, store, session) = fixture().await;
    store
        .append(session, vec![event("alice", "join", "00:01:00")])
        .await
        .unwrap();
    store
        .update_channel("example".into(), false, true)
        .await
        .unwrap();
    let page = timeline(
        &store,
        TimelineQuery {
            account: Some(" ALICE ".into()),
            channel: Some("ExAmPlE".into()),
            ..query()
        },
    )
    .await;
    assert_eq!(page.account, "alice");
    assert_eq!(page.intervals.len(), 1);
    let default = timeline(
        &store,
        TimelineQuery {
            account: Some("alice".into()),
            ..TimelineQuery::default()
        },
    )
    .await;
    assert_eq!(default.limit, 25);
    assert_eq!(
        datetime(&default.effective_range.to)
            .unwrap()
            .signed_duration_since(datetime(&default.effective_range.from).unwrap()),
        Duration::days(7)
    );
    for invalid in [
        TimelineQuery {
            account: None,
            ..query()
        },
        TimelineQuery {
            account: Some("ali*".into()),
            ..query()
        },
        TimelineQuery {
            from: Some("yesterday".into()),
            ..query()
        },
        TimelineQuery {
            from: Some(at("01:00:00")),
            ..query()
        },
        TimelineQuery {
            from: Some("2025-11-01T00:00:00Z".into()),
            ..query()
        },
        TimelineQuery {
            limit: Some(0),
            ..query()
        },
        TimelineQuery {
            limit: Some(101),
            ..query()
        },
        TimelineQuery {
            before: Some(0),
            ..query()
        },
        TimelineQuery {
            broadcast_id: Some(-1),
            ..query()
        },
        TimelineQuery {
            to: Some("+10000-01-01T00:00:00Z".into()),
            ..query()
        },
    ] {
        assert!(matches!(
            store.timeline_at(invalid, at("01:00:00")).await,
            Err(StoreError::Invalid)
        ));
    }
    broadcast(&store).await;
    assert!(matches!(
        store
            .timeline_at(
                TimelineQuery {
                    broadcast_id: Some(7),
                    channel: Some("other".into()),
                    ..query()
                },
                at("01:00:00")
            )
            .await,
        Err(StoreError::Invalid)
    ));
    assert!(matches!(
        store
            .timeline_at(
                TimelineQuery {
                    broadcast_id: Some(999),
                    ..query()
                },
                at("01:00:00")
            )
            .await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn http_rejects_missing_malformed_and_unbounded_queries_with_json_errors() {
    use axum::{body::Body, http::StatusCode, Router};
    use std::sync::Arc;
    use tokio::sync::{watch, Mutex};
    use tower::ServiceExt;
    let (_dir, store, _session) = fixture().await;
    let (channels, _) = watch::channel(Vec::new());
    let router = crate::app::router(
        crate::app::AppState {
            status: crate::collector::status(&store),
            store,
            channels,
            port: 8794,
            configuration: Arc::new(Mutex::new(())),
        },
        Router::new(),
    );
    for path in [
        "/api/timeline",
        "/api/timeline?account=alice&before=oops",
        "/api/timeline?account=alice&limit=101",
        "/api/timeline?account=alice&offset=1",
        "/api/timeline?account=alice&broadcast_id=0",
    ] {
        let response = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri(path)
                    .header("host", "127.0.0.1:8794")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(body["error"]["code"].is_string());
    }
}
