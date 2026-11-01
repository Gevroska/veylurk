use super::*;
use crate::diagnostics::{JobClass, Phase};
use rusqlite::types::Value;
use rusqlite::StatementStatus;
use std::path::Path;

fn provenance() -> CommunityProvenance {
    CommunityProvenance {
        request_after_trigger_verified: true,
        request_after_trigger_ms: Some(2.0),
        request_to_response_ms: Some(12.0),
        request_to_finished_ms: Some(10.0),
        finished_minus_response_ms: Some(-2.0),
        from_disk_cache: Some(false),
        from_service_worker: Some(false),
        from_prefetch_cache: Some(false),
        served_from_cache_event: false,
        encoded_bytes: Some(123),
        transport: CommunityTransport::NetworkResponse,
        server_freshness: CommunityServerFreshness::Unknown,
        http_status: 200,
    }
}

fn success(at: &str, names: &[&str]) -> CommunityCompletion {
    CommunityCompletion::Success {
        received_at_utc: at.into(),
        reported_chatters: Some(12),
        members: names
            .iter()
            .map(|account| CommunityMemberInput {
                account: (*account).into(),
                roles: COMMUNITY_ROLE_VIEWER,
            })
            .collect(),
        categories: CommunityCategoryCounts {
            viewers: names.len() as u32,
            ..CommunityCategoryCounts::default()
        },
        provenance: provenance(),
    }
}

async fn pending(store: &Store, run: i64, job: u64, channel: &str) -> i64 {
    store
        .begin_community_sample(NewCommunitySample {
            run_id: run,
            job_id: job,
            channel: channel.into(),
            started_at_utc: "2024-01-01T00:00:00Z".into(),
        })
        .await
        .unwrap()
}

async fn reopen(path: &Path, store: Store) -> Store {
    drop(store);
    // The actor's channel closes asynchronously; retry only its brief lock-release boundary.
    for _ in 0..100 {
        match Store::open(path).await {
            Ok(store) => return store,
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
        }
    }
    panic!("storage worker did not release its isolated fixture");
}

fn snapshot(conn: &Connection, tables: &[&str]) -> Vec<Vec<Vec<Value>>> {
    tables
        .iter()
        .map(|table| {
            let mut stmt = conn
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .unwrap();
            let columns = stmt.column_count();
            let rows = stmt
                .query_map([], |r| {
                    (0..columns)
                        .map(|column| r.get(column))
                        .collect::<Result<Vec<Value>, _>>()
                })
                .unwrap();
            rows.collect::<Result<Vec<_>, _>>().unwrap()
        })
        .collect()
}

fn insert_retained_community_rows(conn: &mut Connection, first: i64, last: i64) {
    let tx = conn.transaction().unwrap();
    tx.execute_batch("INSERT OR IGNORE INTO community_runs(id,started_at_utc,build_identity) VALUES(1,'2020-01-01T00:00:00.000Z','retained');").unwrap();
    tx.execute(
        "WITH RECURSIVE n(x) AS(VALUES(?1) UNION ALL SELECT x+1 FROM n WHERE x<?2)
        INSERT INTO community_samples(id,run_id,job_id,channel,started_at_utc,received_at_utc,outcome,returned_accounts,broadcasters,staff,moderators,vips,chatbots,viewers,request_after_trigger_verified,transport,http_status,served_from_cache_event)
        SELECT x,1,x,CASE WHEN x%2=0 THEN 'room' ELSE 'other' END,'2020-01-01T00:00:00.000Z','2020-01-01T00:00:01.000Z','success',1,0,0,0,0,0,1,1,'unknown',200,0 FROM n",
        params![first,last],
    ).unwrap();
    tx.execute(
        "INSERT INTO community_members(id,sample_id,channel,account,roles) SELECT id,id,channel,CASE WHEN id%3=0 THEN 'alice' ELSE 'retained' END,32 FROM community_samples WHERE id BETWEEN ?1 AND ?2",
        params![first,last],
    ).unwrap();
    tx.commit().unwrap();
}

fn v1_fixture(conn: &mut Connection, enabled: bool, interval: u32) {
    // Execute the unchanged released DDL: this table genuinely lacks cadence_mode.
    conn.execute_batch(SCHEMA_V1).unwrap();
    assert_eq!(
        settings_columns(conn),
        ["id", "enabled", "target_interval_seconds"]
    );
    insert_retained_community_rows(conn, 1, 3);
    conn.execute(
        "UPDATE community_settings SET enabled=?1,target_interval_seconds=?2 WHERE id=1",
        params![enabled, interval],
    )
    .unwrap();
    conn.execute_batch(
        "UPDATE community_pause SET reason='integrity_denied' WHERE id=1;
        UPDATE community_runs SET ended_at_utc='2020-01-01T00:00:02.000Z',end_reason='shutdown';",
    )
    .unwrap();
}

fn settings_columns(conn: &Connection) -> Vec<String> {
    conn.prepare("PRAGMA table_info(community_settings)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn component_schema(conn: &Connection) -> Vec<(String, Option<String>)> {
    conn.prepare("SELECT name,sql FROM sqlite_master WHERE name GLOB 'community_*' ORDER BY name")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn v1_and_v2_pause_migration_preserves_each_legacy_reason_and_unrelated_state() {
    for version in [1, 2] {
        for reason in [
            None,
            Some("authentication_denied"),
            Some("integrity_denied"),
            Some("challenge_denied"),
            Some("rate_limited"),
            Some("source_denied"),
        ] {
            let mut conn = Connection::open_in_memory().unwrap();
            v1_fixture(&mut conn, true, 777);
            if version == 2 {
                conn.execute_batch(MIGRATE_V2).unwrap();
                conn.execute("UPDATE community_settings SET cadence_mode='adaptive'", [])
                    .unwrap();
            }
            conn.execute("UPDATE community_pause SET reason=?1", [reason])
                .unwrap();
            conn.execute_batch(
                "CREATE TABLE credential_fixture(id INTEGER PRIMARY KEY,opaque BLOB NOT NULL);
                INSERT INTO credential_fixture VALUES(1,x'001122ff');
                PRAGMA user_version=37;",
            )
            .unwrap();
            let preserved = [
                "community_pause",
                "community_runs",
                "community_samples",
                "community_members",
                "sqlite_sequence",
                "credential_fixture",
            ];
            let before = snapshot(&conn, &preserved);
            let history_schema: Vec<_> = component_schema(&conn)
                .into_iter()
                .filter(|(name, _)| {
                    !matches!(name.as_str(), "community_pause" | "community_settings")
                })
                .filter(|(name, _)| name != "community_members_sample_member")
                .collect();
            initialize(&mut conn).unwrap();
            assert_eq!(snapshot(&conn, &preserved), before, "v{version} {reason:?}");
            assert_eq!(
                read_community_pause(&conn)
                    .unwrap()
                    .map(|reason| reason.code()),
                reason
            );
            let settings = read_community_settings(&conn).unwrap();
            assert!(settings.enabled);
            assert_eq!(settings.target_interval_seconds, 777);
            assert_eq!(
                settings.cadence_mode,
                if version == 2 {
                    CommunityCadenceMode::Adaptive
                } else {
                    CommunityCadenceMode::Fixed
                }
            );
            assert_eq!(
                conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                    .unwrap(),
                37
            );
            // The only history access-path changes are the already approved indexes.
            let after_schema: Vec<_> = component_schema(&conn)
                .into_iter()
                .filter(|(name, _)| {
                    !matches!(
                        name.as_str(),
                        "community_pause"
                            | "community_settings"
                            | "community_members_account_channel_member_sample"
                            | "community_members_sample_member"
                    )
                })
                .collect();
            assert_eq!(after_schema, history_schema);
            conn.execute(
                "UPDATE community_pause SET reason='cleanup_unconfirmed'",
                [],
            )
            .unwrap();
            assert_eq!(
                read_community_pause(&conn).unwrap(),
                Some(CommunityPauseReason::CleanupUnconfirmed)
            );
            assert!(conn
                .execute("UPDATE community_pause SET reason='unknown'", [])
                .is_err());
            let retained = snapshot(&conn, &preserved);
            let schema = component_schema(&conn);
            initialize(&mut conn).unwrap();
            assert_eq!(snapshot(&conn, &preserved), retained);
            assert_eq!(component_schema(&conn), schema);
        }
    }
}

#[test]
fn pause_v3_migration_and_pending_recovery_roll_back_as_one_transaction() {
    for version in [1, 2] {
        for failed_stage in ["version", "recovery"] {
            let mut conn = Connection::open_in_memory().unwrap();
            v1_fixture(&mut conn, true, 777);
            if version == 2 {
                conn.execute_batch(MIGRATE_V2).unwrap();
            }
            conn.execute_batch("UPDATE community_runs SET ended_at_utc=NULL,end_reason=NULL;
                UPDATE community_pause SET reason='source_denied';
                INSERT INTO community_samples(run_id,job_id,channel,started_at_utc,outcome) VALUES(1,4,'room','2020-01-01T00:00:01.000Z','pending');").unwrap();
            if failed_stage == "version" {
                conn.execute_batch("CREATE TRIGGER community_fail_v3 BEFORE UPDATE ON community_schema_version WHEN NEW.version=3 BEGIN SELECT RAISE(ABORT,'fixture v3 version failure'); END;").unwrap();
            } else {
                conn.execute_batch("CREATE TRIGGER community_fail_v3_recovery BEFORE UPDATE OF outcome ON community_samples BEGIN SELECT RAISE(ABORT,'fixture v3 recovery failure'); END;").unwrap();
            }
            let tables = [
                "community_schema_version",
                "community_settings",
                "community_pause",
                "community_runs",
                "community_samples",
                "community_members",
                "sqlite_sequence",
            ];
            let before = snapshot(&conn, &tables);
            let schema = component_schema(&conn);
            assert!(initialize(&mut conn).is_err(), "v{version} {failed_stage}");
            assert_eq!(snapshot(&conn, &tables), before);
            assert_eq!(component_schema(&conn), schema);
            assert!(conn.is_autocommit());
            assert!(
                conn.execute(
                    "UPDATE community_pause SET reason='cleanup_unconfirmed'",
                    []
                )
                .is_err(),
                "old CHECK constraint must survive rollback"
            );
        }
    }
}

#[test]
fn v1_and_v2_pending_recovery_preserves_original_history_and_every_pause() {
    for version in [1, 2] {
        for reason in [
            None,
            Some("authentication_denied"),
            Some("integrity_denied"),
            Some("challenge_denied"),
            Some("rate_limited"),
            Some("source_denied"),
        ] {
            let mut conn = Connection::open_in_memory().unwrap();
            v1_fixture(&mut conn, true, 777);
            if version == 2 {
                conn.execute_batch(MIGRATE_V2).unwrap();
            }
            conn.execute("UPDATE community_pause SET reason=?1", [reason])
                .unwrap();
            let before = snapshot(
                &conn,
                &["community_runs", "community_samples", "community_members"],
            );
            conn.execute_batch("INSERT INTO community_runs(id,started_at_utc,build_identity) VALUES(2,'2020-01-01T00:00:00.000Z','pending-fixture');
                INSERT INTO community_samples(id,run_id,job_id,channel,started_at_utc,outcome) VALUES(4,2,1,'room','2020-01-01T00:00:01.000Z','pending');").unwrap();
            let sequences = snapshot(&conn, &["sqlite_sequence"]);
            initialize(&mut conn).unwrap();
            let after = snapshot(
                &conn,
                &["community_runs", "community_samples", "community_members"],
            );
            assert_eq!(&after[0][..1], &before[0]);
            assert_eq!(&after[1][..3], &before[1]);
            assert_eq!(after[2], before[2]);
            assert_eq!(snapshot(&conn, &["sqlite_sequence"]), sequences);
            let recovered: (String, String, String) = conn.query_row("SELECT r.end_reason,s.outcome,s.failure_code FROM community_samples s JOIN community_runs r ON r.id=s.run_id WHERE s.id=4", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
            assert_eq!(
                recovered,
                (
                    "process_interrupted".into(),
                    "interrupted".into(),
                    "process_interrupted".into()
                )
            );
            assert_eq!(
                read_community_pause(&conn)
                    .unwrap()
                    .map(|reason| reason.code()),
                reason
            );
        }
    }
}

#[test]
fn false_v3_declaration_cannot_skip_the_pause_constraint_migration() {
    let mut conn = Connection::open_in_memory().unwrap();
    v1_fixture(&mut conn, true, 777);
    conn.execute_batch(MIGRATE_V2).unwrap();
    conn.execute_batch("UPDATE community_schema_version SET version=3;
        INSERT INTO community_samples(run_id,job_id,channel,started_at_utc,outcome) VALUES(1,4,'room','2020-01-01T00:00:01.000Z','pending');").unwrap();
    let tables = [
        "community_schema_version",
        "community_settings",
        "community_pause",
        "community_runs",
        "community_samples",
        "community_members",
    ];
    let before = snapshot(&conn, &tables);
    let schema = component_schema(&conn);
    assert!(matches!(initialize(&mut conn), Err(StoreError::Initialize)));
    assert_eq!(snapshot(&conn, &tables), before);
    assert_eq!(component_schema(&conn), schema);
}

#[test]
fn receipt_pair_index_is_additive_for_fresh_v1_and_v2_and_reopen_is_idempotent() {
    for version in [None, Some(1), Some(2)] {
        let mut conn = Connection::open_in_memory().unwrap();
        if let Some(version) = version {
            v1_fixture(&mut conn, true, 777);
            if version == 2 {
                conn.execute_batch(MIGRATE_V2).unwrap();
            }
        }
        let history = [
            "community_pause",
            "community_runs",
            "community_samples",
            "community_members",
        ];
        let retained = version.map(|_| snapshot(&conn, &history));
        initialize(&mut conn).unwrap();
        if let Some(retained) = retained {
            assert_eq!(snapshot(&conn, &history), retained);
            assert_eq!(
                read_community_settings(&conn).unwrap(),
                CommunitySettings {
                    enabled: true,
                    target_interval_seconds: 777,
                    cadence_mode: CommunityCadenceMode::Fixed,
                }
            );
        } else {
            assert_eq!(
                read_community_settings(&conn).unwrap(),
                CommunitySettings::default()
            );
        }
        let columns = conn
            .prepare("PRAGMA index_info(community_members_account_channel_member_sample)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(2))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(columns, ["account", "channel", "id", "sample_id"]);
        assert_eq!(
            conn.query_row("SELECT version FROM community_schema_version", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            COMMUNITY_SCHEMA_VERSION
        );
        let schema = component_schema(&conn);
        let retained = snapshot(&conn, &history);
        initialize(&mut conn).unwrap();
        assert_eq!(component_schema(&conn), schema);
        assert_eq!(snapshot(&conn, &history), retained);
    }
}

#[test]
fn receipt_pair_index_creation_rolls_back_with_failed_recovery() {
    for version in [1, 2] {
        let mut conn = Connection::open_in_memory().unwrap();
        v1_fixture(&mut conn, true, 777);
        if version == 2 {
            conn.execute_batch(MIGRATE_V2).unwrap();
        }
        assert!(sample_member_index_needs_covering(&conn).unwrap());
        conn.execute_batch("UPDATE community_runs SET ended_at_utc=NULL,end_reason=NULL;
            INSERT INTO community_samples(run_id,job_id,channel,started_at_utc,outcome) VALUES(1,4,'room','2020-01-01T00:00:01.000Z','pending');
            CREATE TRIGGER community_fail_recovery BEFORE UPDATE OF outcome ON community_samples BEGIN SELECT RAISE(ABORT,'fixture recovery failure'); END;").unwrap();
        let tables = [
            "community_schema_version",
            "community_settings",
            "community_pause",
            "community_runs",
            "community_samples",
            "community_members",
        ];
        let before = snapshot(&conn, &tables);
        let schema = component_schema(&conn);
        assert!(initialize(&mut conn).is_err());
        assert_eq!(snapshot(&conn, &tables), before);
        assert_eq!(component_schema(&conn), schema);
        assert!(sample_member_index_needs_covering(&conn).unwrap());
        assert!(conn.is_autocommit());
    }
}

#[test]
fn sample_member_covering_upgrade_preserves_history_and_reopen_performs_no_ddl() {
    for version in [None, Some(1), Some(2)] {
        let mut conn = Connection::open_in_memory().unwrap();
        if let Some(version) = version {
            v1_fixture(&mut conn, true, 777);
            if version == 2 {
                conn.execute_batch(MIGRATE_V2).unwrap();
            }
            assert!(sample_member_index_needs_covering(&conn).unwrap());
        }
        let history = [
            "community_pause",
            "community_runs",
            "community_samples",
            "community_members",
        ];
        let retained = version.map(|_| snapshot(&conn, &history));
        initialize(&mut conn).unwrap();
        assert!(!sample_member_index_needs_covering(&conn).unwrap());
        if let Some(retained) = retained {
            assert_eq!(snapshot(&conn, &history), retained);
        }
        let all = [
            "community_schema_version",
            "community_settings",
            "community_pause",
            "community_runs",
            "community_samples",
            "community_members",
        ];
        let retained = snapshot(&conn, &all);
        let schema = component_schema(&conn);
        let schema_cookie: i64 = conn
            .pragma_query_value(None, "schema_version", |row| row.get(0))
            .unwrap();
        initialize(&mut conn).unwrap();
        assert_eq!(snapshot(&conn, &all), retained);
        assert_eq!(component_schema(&conn), schema);
        assert_eq!(
            conn.pragma_query_value(None, "schema_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            schema_cookie,
            "known covering index was rebuilt on reopen"
        );
        assert_eq!(
            conn.query_row("SELECT version FROM community_schema_version", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            COMMUNITY_SCHEMA_VERSION
        );
    }
}

#[test]
fn unexpected_sample_member_index_definitions_fail_before_schema_or_recovery_changes() {
    for definition in [
        "",
        "CREATE TABLE community_members_sample_member(value INTEGER)",
        "CREATE INDEX community_members_sample_member ON community_runs(id,started_at_utc)",
        "CREATE UNIQUE INDEX community_members_sample_member ON community_members(sample_id,id)",
        "CREATE INDEX community_members_sample_member ON community_members(sample_id,id) WHERE roles=32",
        "CREATE INDEX community_members_sample_member ON community_members(id,sample_id)",
        "CREATE INDEX community_members_sample_member ON community_members(sample_id DESC,id)",
        "CREATE INDEX community_members_sample_member ON community_members(sample_id COLLATE NOCASE,id)",
        "CREATE INDEX community_members_sample_member ON community_members(sample_id+0,id)",
        "CREATE INDEX community_members_sample_member ON community_members(sample_id)",
        "CREATE INDEX community_members_sample_member ON community_members(sample_id,id,account)",
        "CREATE INDEX community_members_sample_member ON community_members(sample_id,id,roles,account)",
        "CREATE INDEX community_members_sample_member ON community_members(sample_id,id,roles DESC)",
        "CREATE INDEX community_members_sample_member ON community_members(sample_id,id,roles COLLATE NOCASE)",
    ] {
        for version in [1, 2] {
            let mut conn = Connection::open_in_memory().unwrap();
            v1_fixture(&mut conn, true, 777);
            if version == 2 {
                conn.execute_batch(MIGRATE_V2).unwrap();
            }
            conn.execute_batch("DROP INDEX community_members_sample_member;
                UPDATE community_runs SET ended_at_utc=NULL,end_reason=NULL;
                INSERT INTO community_samples(run_id,job_id,channel,started_at_utc,outcome) VALUES(1,4,'room','2020-01-01T00:00:01.000Z','pending');").unwrap();
            conn.execute_batch(definition).unwrap();
            let tables = [
                "community_schema_version",
                "community_settings",
                "community_pause",
                "community_runs",
                "community_samples",
                "community_members",
            ];
            let before = snapshot(&conn, &tables);
            let schema = component_schema(&conn);
            assert!(matches!(initialize(&mut conn), Err(StoreError::Initialize)), "v{version}: {definition}");
            assert_eq!(snapshot(&conn, &tables), before, "v{version}: {definition}");
            assert_eq!(component_schema(&conn), schema, "v{version}: {definition}");
            assert!(conn.is_autocommit());
        }
    }
}

#[tokio::test]
async fn actual_v1_migration_preserves_preferences_pause_and_history_and_reopen_is_idempotent() {
    let directory = tempfile::tempdir().unwrap();
    for (enabled, interval) in [(false, 30), (true, 777)] {
        let path = directory.path().join(format!("v1-{enabled}.sqlite3"));
        let mut conn = Connection::open(&path).unwrap();
        v1_fixture(&mut conn, enabled, interval);
        let history = [
            "community_pause",
            "community_runs",
            "community_samples",
            "community_members",
        ];
        let retained = snapshot(&conn, &history);
        drop(conn);
        let store = Store::open(&path).await.unwrap();
        assert_eq!(
            store.community_settings().await.unwrap(),
            CommunitySettings {
                enabled,
                target_interval_seconds: interval,
                cadence_mode: CommunityCadenceMode::Fixed,
            }
        );
        assert_eq!(
            store.community_pause().await.unwrap(),
            Some(CommunityPauseReason::IntegrityDenied)
        );
        assert_eq!(
            store.community_stats().await.unwrap().schema_version,
            COMMUNITY_SCHEMA_VERSION
        );
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            settings_columns(&conn),
            ["id", "enabled", "target_interval_seconds", "cadence_mode"]
        );
        assert_eq!(snapshot(&conn, &history), retained);
        let all = [
            "community_schema_version",
            "community_settings",
            "community_pause",
            "community_runs",
            "community_samples",
            "community_members",
        ];
        let migrated = snapshot(&conn, &all);
        let schema = component_schema(&conn);
        drop(conn);
        let store = reopen(&path, store).await;
        let conn = Connection::open(&path).unwrap();
        assert_eq!(snapshot(&conn, &all), migrated);
        assert_eq!(component_schema(&conn), schema);
        assert_eq!(
            store
                .community_receipts(CommunityReceiptQuery::default())
                .await
                .unwrap()
                .rows
                .len(),
            3
        );
    }
}

#[tokio::test]
async fn fresh_install_is_disabled_adaptive_and_typed_modes_round_trip_without_clearing_pause() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cadence.sqlite3");
    let mut store = Store::open(&path).await.unwrap();
    assert_eq!(
        store.community_settings().await.unwrap(),
        CommunitySettings {
            enabled: false,
            target_interval_seconds: 300,
            cadence_mode: CommunityCadenceMode::Adaptive,
        }
    );
    store
        .set_community_pause(Some(CommunityPauseReason::SourceDenied))
        .await
        .unwrap();
    for cadence_mode in [CommunityCadenceMode::Fixed, CommunityCadenceMode::Adaptive] {
        let settings = CommunitySettings {
            enabled: true,
            target_interval_seconds: 600,
            cadence_mode,
        };
        store.set_community_settings(settings).await.unwrap();
        store = reopen(&path, store).await;
        assert_eq!(store.community_settings().await.unwrap(), settings);
        assert_eq!(
            store.community_pause().await.unwrap(),
            Some(CommunityPauseReason::SourceDenied)
        );
        assert_eq!(
            serde_json::to_value(cadence_mode).unwrap(),
            cadence_mode.code()
        );
    }
    for json in [
        r#"{"enabled":true,"target_interval_seconds":300}"#,
        r#"{"enabled":true,"target_interval_seconds":300,"cadence_mode":"unknown"}"#,
    ] {
        assert!(serde_json::from_str::<CommunitySettings>(json).is_err());
    }
}

#[test]
fn malformed_version_and_false_v2_declarations_are_rejected_without_mutation() {
    for values in [
        "",
        "(1,4)",
        "(1,0)",
        "(1,-1)",
        "(1,NULL)",
        "(1,'malformed')",
        "(1,1.5)",
        "(2,1)",
        "(1,1),(2,1)",
        "(1,1),(1,1)",
    ] {
        let mut conn = Connection::open_in_memory().unwrap();
        v1_fixture(&mut conn, true, 777);
        conn.execute_batch("DROP TABLE community_schema_version;CREATE TABLE community_schema_version(id,version);").unwrap();
        if !values.is_empty() {
            conn.execute_batch(&format!(
                "INSERT INTO community_schema_version VALUES {values};"
            ))
            .unwrap();
        }
        let tables = [
            "community_schema_version",
            "community_settings",
            "community_pause",
            "community_runs",
            "community_samples",
            "community_members",
        ];
        let before = snapshot(&conn, &tables);
        let schema = component_schema(&conn);
        assert!(
            matches!(initialize(&mut conn), Err(StoreError::Initialize)),
            "{values}"
        );
        assert_eq!(snapshot(&conn, &tables), before, "{values}");
        assert_eq!(component_schema(&conn), schema, "{values}");
    }
    let mut conn = Connection::open_in_memory().unwrap();
    v1_fixture(&mut conn, true, 777);
    conn.execute("UPDATE community_schema_version SET version=2", [])
        .unwrap();
    let schema = component_schema(&conn);
    let before = snapshot(
        &conn,
        &[
            "community_schema_version",
            "community_settings",
            "community_pause",
        ],
    );
    assert!(matches!(initialize(&mut conn), Err(StoreError::Initialize)));
    assert_eq!(component_schema(&conn), schema);
    assert_eq!(
        snapshot(
            &conn,
            &[
                "community_schema_version",
                "community_settings",
                "community_pause"
            ]
        ),
        before
    );
}

#[test]
fn failed_migration_rolls_back_added_column_version_and_recovery() {
    for failure in ["invalid_pause", "invalid_mode", "version_write"] {
        let mut conn = Connection::open_in_memory().unwrap();
        v1_fixture(&mut conn, true, 777);
        conn.execute_batch("UPDATE community_runs SET ended_at_utc=NULL,end_reason=NULL;
            INSERT INTO community_samples(run_id,job_id,channel,started_at_utc,outcome) VALUES(1,4,'room','2020-01-01T00:00:01.000Z','pending');").unwrap();
        if failure == "invalid_pause" {
            conn.execute_batch("PRAGMA ignore_check_constraints=ON;UPDATE community_pause SET reason='unknown';PRAGMA ignore_check_constraints=OFF;").unwrap();
        } else if failure == "invalid_mode" {
            conn.execute_batch("PRAGMA ignore_check_constraints=ON;
                CREATE TRIGGER community_invalid_mode AFTER UPDATE ON community_schema_version BEGIN UPDATE community_settings SET cadence_mode='unknown'; END;").unwrap();
        } else {
            // The ALTER succeeds before this injected version-update failure.
            conn.execute_batch("CREATE TRIGGER community_fail_version BEFORE UPDATE ON community_schema_version BEGIN SELECT RAISE(ABORT,'fixture version update'); END;").unwrap();
        }
        let tables = [
            "community_schema_version",
            "community_settings",
            "community_pause",
            "community_runs",
            "community_samples",
            "community_members",
        ];
        let before = snapshot(&conn, &tables);
        let schema = component_schema(&conn);
        assert!(initialize(&mut conn).is_err(), "{failure}");
        assert_eq!(snapshot(&conn, &tables), before, "{failure}");
        assert_eq!(component_schema(&conn), schema, "{failure}");
        assert_eq!(
            settings_columns(&conn),
            ["id", "enabled", "target_interval_seconds"]
        );
        assert!(conn.is_autocommit());
    }
}

#[test]
fn invalid_retained_v2_mode_blocks_recovery_without_mutation() {
    let mut conn = Connection::open_in_memory().unwrap();
    initialize(&mut conn).unwrap();
    assert!(conn
        .execute("UPDATE community_settings SET cadence_mode='unknown'", [])
        .is_err());
    conn.execute_batch("INSERT INTO community_runs VALUES(1,'2024-01-01T00:00:00.000Z',NULL,NULL,'fixture');
        INSERT INTO community_samples(run_id,job_id,channel,started_at_utc,outcome) VALUES(1,1,'room','2024-01-01T00:00:00.000Z','pending');
        UPDATE community_pause SET reason='integrity_denied';
        PRAGMA ignore_check_constraints=ON;UPDATE community_settings SET cadence_mode='unknown';PRAGMA ignore_check_constraints=OFF;").unwrap();
    let tables = [
        "community_schema_version",
        "community_settings",
        "community_pause",
        "community_runs",
        "community_samples",
    ];
    let before = snapshot(&conn, &tables);
    let schema = component_schema(&conn);
    assert!(matches!(
        read_community_settings(&conn),
        Err(StoreError::Invalid)
    ));
    assert!(matches!(initialize(&mut conn), Err(StoreError::Initialize)));
    assert_eq!(snapshot(&conn, &tables), before);
    assert_eq!(component_schema(&conn), schema);
    assert!(conn.is_autocommit());
}

#[tokio::test]
async fn additive_migration_and_reopen_preserve_every_legacy_record_and_model_version() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy.sqlite3");
    let mut conn = Connection::open(&path).unwrap();
    super::super::initialize_schema(&mut conn).unwrap();
    conn.execute_batch("INSERT INTO channels VALUES('room',1,1,'2024-01-01T00:00:00Z','2024-01-01T00:00:00Z');
        INSERT INTO sessions(id,started_at,ended_at,end_reason) VALUES(1,'2024-01-01T00:00:00Z','2024-01-01T00:00:04Z','fixture_end');
        INSERT INTO session_channels(session_id,channel) VALUES(1,'room');
        INSERT INTO observations(session_id,channel,account,kind,received_at) VALUES(1,'room','alice','join','2024-01-01T01:00:01+01:00'),(1,'room','alice','part','2024-01-01T00:00:03Z');
        INSERT INTO relationships VALUES('room','alice','2024-01-01T00:00:01Z','2024-01-01T00:00:03Z',2);
        UPDATE collection_totals SET observation_count=2,session_count=1;").unwrap();
    crate::history::initialize(&mut conn).unwrap();
    let tables = [
        "channels",
        "sessions",
        "session_channels",
        "observations",
        "relationships",
        "collection_totals",
        "history_visits",
        "history_boundaries",
        "history_model",
    ];
    let before = snapshot(&conn, &tables);
    drop(conn);
    let store = Store::open(&path).await.unwrap();
    assert_eq!(
        store.community_settings().await.unwrap(),
        CommunitySettings::default()
    );
    let run = store
        .begin_community_run("fixture-build".into())
        .await
        .unwrap();
    let sample = pending(&store, run, 1, "room").await;
    store
        .complete_community_sample(sample, success("2024-01-01T00:00:02Z", &["community_only"]))
        .await
        .unwrap();
    store
        .end_community_run(run, CommunityRunEndReason::Shutdown)
        .await
        .unwrap();
    let store = reopen(&path, store).await;
    let conn = Connection::open(&path).unwrap();
    assert_eq!(snapshot(&conn, &tables), before);
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        crate::history::checked_model_version(&conn).unwrap(),
        Some(crate::history::MODEL_VERSION)
    );
    assert_eq!(store.community_stats().await.unwrap().member_count, 1);
    assert_eq!(store.stats().await.unwrap().observation_count, 2);
    assert!(store
        .search(Some("community_only".into()), None, None, 10)
        .await
        .unwrap()
        .is_empty());
}

#[test]
fn unknown_component_version_is_rejected_before_component_creation_or_recovery() {
    let mut conn = Connection::open_in_memory().unwrap();
    initialize(&mut conn).unwrap();
    conn.execute_batch("INSERT INTO community_runs VALUES(1,'2024-01-01T00:00:00Z',NULL,NULL,'fixture');
        INSERT INTO community_samples(run_id,job_id,channel,started_at_utc,outcome) VALUES(1,1,'room','2024-01-01T00:00:00Z','pending');
        UPDATE community_schema_version SET version=4;").unwrap();
    let tables = [
        "community_schema_version",
        "community_settings",
        "community_pause",
        "community_runs",
        "community_samples",
        "community_members",
    ];
    let before = snapshot(&conn, &tables);
    let schema: String = conn
        .query_row("SELECT group_concat(sql) FROM sqlite_master", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(matches!(initialize(&mut conn), Err(StoreError::Initialize)));
    assert_eq!(snapshot(&conn, &tables), before);
    assert_eq!(
        conn.query_row("SELECT group_concat(sql) FROM sqlite_master", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        schema
    );
    conn.execute_batch("DROP TABLE community_schema_version;")
        .unwrap();
    assert!(matches!(initialize(&mut conn), Err(StoreError::Initialize)));
    assert!(!conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='community_schema_version')",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
}

#[tokio::test]
async fn durable_pause_survives_recovery_and_preference_changes_until_explicit_clear() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pause.sqlite3");
    let mut store = Store::open(&path).await.unwrap();
    assert_eq!(store.community_pause().await.unwrap(), None);
    let settings = CommunitySettings {
        enabled: true,
        target_interval_seconds: 600,
        cadence_mode: CommunityCadenceMode::Fixed,
    };
    store.set_community_settings(settings).await.unwrap();
    for reason in [
        CommunityPauseReason::AuthenticationDenied,
        CommunityPauseReason::IntegrityDenied,
        CommunityPauseReason::ChallengeDenied,
        CommunityPauseReason::RateLimited,
        CommunityPauseReason::SourceDenied,
        CommunityPauseReason::CleanupUnconfirmed,
    ] {
        store.set_community_pause(Some(reason)).await.unwrap();
        assert_eq!(store.community_pause().await.unwrap(), Some(reason));
        assert_eq!(serde_json::to_value(reason).unwrap(), reason.code());
        let run = store
            .begin_community_run("pause-fixture".into())
            .await
            .unwrap();
        pending(&store, run, 1, "room").await;
        store = reopen(&path, store).await;
        assert_eq!(store.community_pause().await.unwrap(), Some(reason));
        assert_eq!(store.community_settings().await.unwrap(), settings);
        assert_eq!(store.community_stats().await.unwrap().pending_count, 0);
        store
            .set_community_settings(CommunitySettings {
                enabled: false,
                ..settings
            })
            .await
            .unwrap();
        store.set_community_settings(settings).await.unwrap();
        let run = store
            .begin_community_run("sign-in-fixture".into())
            .await
            .unwrap();
        store
            .end_community_run(run, CommunityRunEndReason::SignIn)
            .await
            .unwrap();
        assert_eq!(store.community_pause().await.unwrap(), Some(reason));
    }
    store.set_community_pause(None).await.unwrap();
    store = reopen(&path, store).await;
    assert_eq!(store.community_pause().await.unwrap(), None);
    assert_eq!(store.community_settings().await.unwrap(), settings);
}

#[test]
fn unknown_pause_code_is_rejected_without_recovery_or_silent_clear() {
    let mut conn = Connection::open_in_memory().unwrap();
    initialize(&mut conn).unwrap();
    conn.execute(
        "UPDATE community_pause SET reason='integrity_denied' WHERE id=1",
        [],
    )
    .unwrap();
    assert!(conn
        .execute(
            "UPDATE community_pause SET reason='unexpected' WHERE id=1",
            []
        )
        .is_err());
    assert_eq!(
        read_community_pause(&conn).unwrap(),
        Some(CommunityPauseReason::IntegrityDenied)
    );
    assert!(conn
        .execute("INSERT INTO community_pause(id,reason) VALUES(2,NULL)", [])
        .is_err());
    assert!(serde_json::from_str::<CommunityPauseReason>("\"unexpected\"").is_err());
    // Simulate corrupted retained data despite the normal schema constraint.
    conn.execute_batch("INSERT INTO community_runs VALUES(1,'2024-01-01T00:00:00.000Z',NULL,NULL,'fixture');
        INSERT INTO community_samples(run_id,job_id,channel,started_at_utc,outcome) VALUES(1,1,'room','2024-01-01T00:00:00.000Z','pending');
        PRAGMA ignore_check_constraints=ON;
        UPDATE community_pause SET reason='unexpected' WHERE id=1;
        PRAGMA ignore_check_constraints=OFF;").unwrap();
    let tables = [
        "community_pause",
        "community_settings",
        "community_runs",
        "community_samples",
    ];
    let before = snapshot(&conn, &tables);
    assert!(matches!(
        read_community_pause(&conn),
        Err(StoreError::Invalid)
    ));
    assert!(matches!(initialize(&mut conn), Err(StoreError::Initialize)));
    assert_eq!(snapshot(&conn, &tables), before);
    assert!(conn.is_autocommit());
}

#[tokio::test]
async fn success_is_atomic_deduplicates_normalized_members_and_never_collapses_requests() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("atomic.sqlite3"))
        .await
        .unwrap();
    let run = store.begin_community_run("fixture".into()).await.unwrap();
    let sample = pending(&store, run, 1, "Room").await;
    store.call_write(Operation::CommunityAppend,|conn| {
        conn.execute_batch("CREATE TRIGGER fail_second_community_member BEFORE INSERT ON community_members WHEN NEW.account='bob' BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;
        Ok(())
    }).await.unwrap();
    let error = store
        .complete_community_sample(sample, success("2024-01-01T00:00:10Z", &["alice", "bob"]))
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::Sqlite(_)));
    let stats = store.community_stats().await.unwrap();
    assert_eq!(
        (
            stats.pending_count,
            stats.successful_count,
            stats.member_count
        ),
        (1, 0, 0)
    );
    let diagnostic = store.diagnostics().first_failure.unwrap();
    assert_eq!(diagnostic.operation, Operation::CommunityAppend);
    assert_eq!(diagnostic.job_class, Some(JobClass::DurableMutation));
    assert!(!diagnostic.progress_interrupt_requested);
    assert_eq!(diagnostic.autocommit_after, Some(true));
    store
        .call_write(Operation::CommunityAppend, |conn| {
            conn.execute_batch("DROP TRIGGER fail_second_community_member;")?;
            Ok(())
        })
        .await
        .unwrap();
    let mut completed = success("2024-01-01T01:00:10+01:00", &[" ALICE ", "Alice", "bob"]);
    if let CommunityCompletion::Success {
        members,
        categories,
        ..
    } = &mut completed
    {
        members[1].roles = COMMUNITY_ROLE_MODERATOR;
        categories.moderators = 1;
        categories.viewers = 2;
    }
    store
        .complete_community_sample(sample, completed.clone())
        .await
        .unwrap();
    assert!(store
        .complete_community_sample(sample, completed)
        .await
        .is_err());
    assert!(store
        .begin_community_sample(NewCommunitySample {
            run_id: run,
            job_id: 1,
            channel: "room".into(),
            started_at_utc: "2024-01-01T00:00:00Z".into()
        })
        .await
        .is_err());
    let second = pending(&store, run, 2, "room").await;
    store
        .complete_community_sample(second, success("2024-01-01T00:00:11Z", &["alice", "bob"]))
        .await
        .unwrap();
    let rows = store
        .community_receipts(CommunityReceiptQuery {
            channel: Some(" ROOM ".into()),
            ..CommunityReceiptQuery::default()
        })
        .await
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows.iter()
            .find(|row| row.sample_id == sample && row.account == "alice")
            .unwrap()
            .roles,
        COMMUNITY_ROLE_VIEWER | COMMUNITY_ROLE_MODERATOR
    );
    assert_eq!(rows.iter().filter(|row| row.account == "alice").count(), 2);
    assert!(rows.iter().all(|row| row.source == "community"
        && row.channel == "room"
        && row.received_at_utc.ends_with('Z')
        && row.returned_accounts == 2));
}

#[tokio::test]
async fn failed_cancelled_and_recovered_attempts_have_no_fabricated_members() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("restart.sqlite3");
    let store = Store::open(&path).await.unwrap();
    let run = store.begin_community_run("fixture".into()).await.unwrap();
    let success_id = pending(&store, run, 1, "room").await;
    store
        .complete_community_sample(success_id, success("2024-01-01T00:00:10Z", &["alice"]))
        .await
        .unwrap();
    let failed = pending(&store, run, 2, "room").await;
    store
        .complete_community_sample(
            failed,
            CommunityCompletion::Failed {
                completed_at_utc: "2024-01-01T00:00:11Z".into(),
                code: CommunityFailureCode::IntegrityDenied,
            },
        )
        .await
        .unwrap();
    let cancelled = pending(&store, run, 3, "room").await;
    store
        .complete_community_sample(
            cancelled,
            CommunityCompletion::Cancelled {
                completed_at_utc: "2024-01-01T00:00:12Z".into(),
            },
        )
        .await
        .unwrap();
    let interrupted = pending(&store, run, 4, "room").await;
    let store = reopen(&path, store).await;
    let stats = store.community_stats().await.unwrap();
    assert_eq!(
        (
            stats.pending_count,
            stats.successful_count,
            stats.failed_count,
            stats.cancelled_count,
            stats.interrupted_count,
            stats.member_count
        ),
        (0, 1, 1, 1, 1, 1)
    );
    assert!(store
        .complete_community_sample(interrupted, success("2024-01-01T00:00:13Z", &["ghost"]))
        .await
        .is_err());
    assert!(store
        .begin_community_sample(NewCommunitySample {
            run_id: run,
            job_id: 5,
            channel: "room".into(),
            started_at_utc: "2024-01-01T00:00:00Z".into()
        })
        .await
        .is_err());
    let state = store.call_read(Operation::Query,move |conn| {
        Ok(conn.query_row("SELECT outcome,failure_code,received_at_utc,returned_accounts,(SELECT end_reason FROM community_runs WHERE id=?2) FROM community_samples WHERE id=?1",params![interrupted,run],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<i64>>(3)?,r.get::<_,String>(4)?)))?)
    }).await.unwrap();
    assert_eq!(
        state,
        (
            "interrupted".into(),
            "process_interrupted".into(),
            None,
            None,
            "process_interrupted".into()
        )
    );
    let run = store.begin_community_run("next".into()).await.unwrap();
    let sample = pending(&store, run, 1, "room").await;
    store
        .end_community_run(run, CommunityRunEndReason::Disabled)
        .await
        .unwrap();
    assert!(store
        .complete_community_sample(sample, success("2024-01-01T00:00:14Z", &["ghost"]))
        .await
        .is_err());
    assert_eq!(store.community_stats().await.unwrap().cancelled_count, 2);
}

#[tokio::test]
async fn malformed_inputs_fail_without_changing_pending_rows_or_settings() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("invalid.sqlite3"))
        .await
        .unwrap();
    assert!(store
        .begin_community_run("https://secret.example/".into())
        .await
        .is_err());
    for interval in [0, 29, 86401, u32::MAX] {
        assert!(store
            .set_community_settings(CommunitySettings {
                enabled: true,
                target_interval_seconds: interval,
                cadence_mode: CommunityCadenceMode::Fixed,
            })
            .await
            .is_err());
    }
    assert_eq!(
        store.community_settings().await.unwrap(),
        CommunitySettings::default()
    );
    store
        .set_community_settings(CommunitySettings {
            enabled: true,
            target_interval_seconds: 600,
            cadence_mode: CommunityCadenceMode::Fixed,
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .community_settings()
            .await
            .unwrap()
            .target_interval_seconds,
        600
    );
    let run = store.begin_community_run("fixture".into()).await.unwrap();
    let sample = pending(&store, run, 1, "room").await;
    for mask in [0, 64, u32::MAX] {
        let mut value = success("2024-01-01T00:00:01Z", &["alice"]);
        if let CommunityCompletion::Success { members, .. } = &mut value {
            members[0].roles = mask;
        }
        assert!(matches!(
            store.complete_community_sample(sample, value).await,
            Err(StoreError::Invalid)
        ));
    }
    for index in 0..10 {
        let mut value = success("2024-01-01T00:00:01Z", &["alice"]);
        if let CommunityCompletion::Success {
            provenance: p,
            categories,
            ..
        } = &mut value
        {
            match index {
                0 => p.request_after_trigger_verified = false,
                1 => p.request_to_response_ms = Some(f64::NAN),
                2 => p.request_to_finished_ms = Some(-1.0),
                3 => p.finished_minus_response_ms = Some(f64::INFINITY),
                4 => p.from_disk_cache = Some(true),
                5 => p.from_service_worker = None,
                6 => p.encoded_bytes = None,
                7 => p.http_status = 403,
                8 => p.transport = CommunityTransport::Unknown,
                _ => categories.viewers = 0,
            }
        }
        assert!(matches!(
            store.complete_community_sample(sample, value).await,
            Err(StoreError::Invalid)
        ));
    }
    for at in ["invalid", "2023-01-01T00:00:00Z", "+10000-01-01T00:00:00Z"] {
        assert!(store
            .complete_community_sample(sample, success(at, &["alice"]))
            .await
            .is_err());
    }
    assert!(store
        .complete_community_sample(sample, success("2024-01-01T00:00:01Z", &["invalid-name"]))
        .await
        .is_err());
    let stats = store.community_stats().await.unwrap();
    assert_eq!((stats.pending_count, stats.member_count), (1, 0));
    // Missing cache/wire information remains explicitly unknown, while a signed finish delta is retained.
    let mut value = success("2024-01-01T00:00:01Z", &["alice"]);
    if let CommunityCompletion::Success { provenance: p, .. } = &mut value {
        p.encoded_bytes = None;
        p.from_disk_cache = None;
        p.transport = CommunityTransport::Unknown;
    }
    store
        .complete_community_sample(sample, value)
        .await
        .unwrap();
    let row = store
        .community_receipts(CommunityReceiptQuery::default())
        .await
        .unwrap()
        .rows
        .remove(0);
    assert_eq!(row.provenance.transport, CommunityTransport::Unknown);
    assert_eq!(row.provenance.finished_minus_response_ms, Some(-2.0));
    assert!(serde_json::from_str::<CommunitySettings>(
        r#"{"enabled":true,"target_interval_seconds":300,"cadence_mode":"fixed","cookie":"secret"}"#
    )
    .is_err());
    assert!(serde_json::from_str::<CommunityProvenance>(r#"{"headers":{}}"#).is_err());
}

#[tokio::test]
async fn source_cursor_pages_are_exact_and_historical_ranges_and_broadcast_clipping_agree() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("pages.sqlite3"))
        .await
        .unwrap();
    let run = store.begin_community_run("fixture".into()).await.unwrap();
    for (job, channel, at) in [
        (1, "room", "2024-01-01T00:00:01Z"),
        (2, "other", "2024-01-01T00:00:02Z"),
        (3, "room", "2024-01-01T00:00:02Z"),
        (4, "room", "2024-01-01T00:00:03Z"),
    ] {
        let sample = pending(&store, run, job, channel).await;
        store
            .complete_community_sample(sample, success(at, &["alice", "bob"]))
            .await
            .unwrap();
    }
    let base = CommunityReceiptQuery {
        account: Some(" ALICE ".into()),
        channel: Some("ROOM".into()),
        from_utc: Some("2020-01-01T00:00:00Z".into()),
        to_utc: Some("2025-01-01T00:00:00Z".into()),
        limit: 1,
        ..CommunityReceiptQuery::default()
    };
    let mut query = base.clone();
    let mut ids = Vec::new();
    loop {
        let page = store.community_receipts(query.clone()).await.unwrap();
        assert_eq!(page.rows.len(), 1);
        ids.push(page.rows[0].member_id);
        if let Some(before) = page.next_before_member_id {
            query.before_member_id = Some(before);
        } else {
            break;
        }
    }
    assert_eq!(ids.len(), 3);
    assert!(ids.windows(2).all(|window| window[0] > window[1]));
    let all = store
        .community_receipts(CommunityReceiptQuery {
            limit: 100,
            ..base.clone()
        })
        .await
        .unwrap();
    assert_eq!(
        all.rows.iter().map(|row| row.member_id).collect::<Vec<_>>(),
        ids
    );
    assert!(store
        .community_receipts(CommunityReceiptQuery::default())
        .await
        .unwrap()
        .from_utc
        .is_none());
    store.call_write(Operation::MetadataInitialize,|conn| {
        conn.execute_batch("CREATE TABLE metadata_broadcasts(id INTEGER PRIMARY KEY,channel TEXT,started_at TEXT,last_live_at TEXT);
            INSERT INTO metadata_broadcasts VALUES(9,'room','2024-01-01T00:00:02.000Z','2024-01-01T00:00:03.000Z');")?;
        Ok(())
    }).await.unwrap();
    let page = store
        .community_receipts(CommunityReceiptQuery {
            account: Some("alice".into()),
            broadcast_id: Some(9),
            ..CommunityReceiptQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 2);
    assert_eq!(page.from_utc.as_deref(), Some("2024-01-01T00:00:02.000Z"));
    assert_eq!(page.to_utc.as_deref(), Some("2024-01-01T00:00:03.001Z"));
    let channel_only = store
        .community_receipts(CommunityReceiptQuery {
            broadcast_id: Some(9),
            to_utc: Some("2024-01-01T00:00:03Z".into()),
            ..CommunityReceiptQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(channel_only.rows.len(), 2);
    assert!(channel_only
        .rows
        .iter()
        .all(|row| row.channel == "room" && row.received_at_utc == "2024-01-01T00:00:02.000Z"));
    assert!(store
        .community_receipts(CommunityReceiptQuery {
            channel: Some("other".into()),
            broadcast_id: Some(9),
            ..CommunityReceiptQuery::default()
        })
        .await
        .is_err());
    assert!(store
        .community_receipts(CommunityReceiptQuery {
            broadcast_id: Some(9),
            from_utc: Some("2024-01-01T00:00:04Z".into()),
            ..CommunityReceiptQuery::default()
        })
        .await
        .unwrap()
        .rows
        .is_empty());
    for query in [
        CommunityReceiptQuery {
            limit: 0,
            ..CommunityReceiptQuery::default()
        },
        CommunityReceiptQuery {
            limit: 101,
            ..CommunityReceiptQuery::default()
        },
        CommunityReceiptQuery {
            before_member_id: Some(0),
            ..CommunityReceiptQuery::default()
        },
        CommunityReceiptQuery {
            from_utc: Some("bad".into()),
            ..CommunityReceiptQuery::default()
        },
    ] {
        assert!(store.community_receipts(query).await.is_err());
    }
}

#[tokio::test]
async fn retained_account_and_channel_queries_use_component_indexes_and_query_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("retained.sqlite3"))
        .await
        .unwrap();
    store
        .call_write(Operation::CommunityAppend, |conn| {
            insert_retained_community_rows(conn, 1, 3000);
            Ok(())
        })
        .await
        .unwrap();
    let plans = store
        .call_read(Operation::Query, |conn| {
            [
                receipt_sql(true, true, true, true),
                receipt_sql(false, true, true, true),
                receipt_sql(true, false, true, true),
            ]
            .iter()
            .map(|sql| {
                let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                let rows = stmt.query_map(
                    params![
                        if sql.contains("m.account=?1") {
                            Some("alice")
                        } else {
                            None
                        },
                        if sql.contains("m.channel=?2") {
                            Some("room")
                        } else {
                            None
                        },
                        "2020-01-01T00:00:00.000Z",
                        "2020-01-02T00:00:00.000Z",
                        i64::MAX,
                        11
                    ],
                    |r| r.get::<_, String>(3),
                )?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .collect::<Result<Vec<Vec<String>>, StoreError>>()
        })
        .await
        .unwrap();
    assert!(
        plans[0].iter().any(|plan| plan.contains(
            "SEARCH m USING INDEX community_members_account_channel_member_sample (account=? AND channel=? AND id<?)"
        )),
        "{:?}",
        plans[0]
    );
    assert!(
        plans[1].iter().any(|plan| plan.contains(
            "SEARCH m USING INDEX community_members_channel_member (channel=? AND id<?)"
        )),
        "{:?}",
        plans[1]
    );
    assert!(
        plans[2].iter().any(|plan| plan.contains(
            "SEARCH m USING INDEX community_members_account_member_sample (account=? AND id<?)"
        )),
        "{:?}",
        plans[2]
    );
    assert!(plans
        .iter()
        .flatten()
        .all(|plan| !plan.contains("TEMP B-TREE")));
    assert_eq!(
        store
            .community_receipts(CommunityReceiptQuery {
                account: Some("alice".into()),
                channel: Some("room".into()),
                limit: 10,
                ..CommunityReceiptQuery::default()
            })
            .await
            .unwrap()
            .rows
            .len(),
        10
    );
    assert_eq!(
        store
            .community_receipts(CommunityReceiptQuery {
                channel: Some("room".into()),
                limit: 10,
                ..CommunityReceiptQuery::default()
            })
            .await
            .unwrap()
            .rows
            .len(),
        10
    );
    store.expire_next_query_for_test();
    // An empty historical range deliberately examines enough retained candidates to
    // reach the unchanged progress-handler interval; a seekable full page does not.
    assert!(store
        .community_receipts(CommunityReceiptQuery {
            channel: Some("room".into()),
            from_utc: Some("2021-01-01T00:00:00Z".into()),
            limit: 100,
            ..CommunityReceiptQuery::default()
        })
        .await
        .is_err());
    // The expired query's handler must not reach a durable Community write.
    store
        .set_community_settings(CommunitySettings {
            enabled: true,
            target_interval_seconds: 300,
            cadence_mode: CommunityCadenceMode::Fixed,
        })
        .await
        .unwrap();
    assert!(store.community_settings().await.unwrap().enabled);
    assert!(
        store
            .diagnostics()
            .first_failure
            .unwrap()
            .progress_interrupt_requested
    );
}

#[test]
fn receipt_pair_seek_preserves_pages_and_ranges_without_scanning_unrelated_memberships() {
    let mut conn = Connection::open_in_memory().unwrap();
    initialize(&mut conn).unwrap();
    insert_retained_community_rows(&mut conn, 1000, 1199);
    conn.execute(
        "UPDATE community_samples SET received_at_utc='2020-01-01T00:00:02.000Z' WHERE id%12=0",
        [],
    )
    .unwrap();
    let read_page = |conn: &Connection, sql: &str, before: i64, ranged: bool| {
        let mut statement = conn.prepare(sql).unwrap();
        let rows = statement
            .query_map(
                params![
                    "alice",
                    "room",
                    ranged.then_some("2020-01-01T00:00:01.000Z"),
                    ranged.then_some("2020-01-01T00:00:02.000Z"),
                    before,
                    11
                ],
                receipt_row,
            )
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0);
        assert_eq!(statement.get_status(StatementStatus::Sort), 0);
        (rows, statement.get_status(StatementStatus::VmStep))
    };
    let mut cases = Vec::new();
    for ranged in [false, true] {
        let sql = receipt_sql(true, true, ranged, ranged);
        let expected: Vec<i64> = (0_usize..33)
            .filter(|offset| !ranged || offset.is_multiple_of(2))
            .map(|offset| 1194 - 6 * offset as i64)
            .collect();
        let (first, first_steps) = read_page(&conn, &sql, i64::MAX, ranged);
        assert_eq!(
            first.iter().map(|row| row.member_id).collect::<Vec<_>>(),
            expected[..11]
        );
        let cursor = first[9].member_id;
        let (second, second_steps) = read_page(&conn, &sql, cursor, ranged);
        assert_eq!(
            second.iter().map(|row| row.member_id).collect::<Vec<_>>(),
            expected[10..expected.len().min(21)]
        );
        for (before, rows, steps) in [
            (i64::MAX, first, first_steps),
            (cursor, second, second_steps),
        ] {
            cases.push((
                sql.clone(),
                ranged,
                before,
                serde_json::to_value(rows).unwrap(),
                steps,
            ));
        }
    }
    // High IDs for the same account on another channel and another account on the
    // selected channel must not add filtering work to either cursor page.
    insert_retained_community_rows(&mut conn, 1, 999);
    insert_retained_community_rows(&mut conn, 1200, 30000);
    conn.execute("UPDATE community_members SET account=CASE WHEN channel='room' THEN 'noise' ELSE 'alice' END WHERE id<1000 OR id>=1200", []).unwrap();
    for (sql, ranged, before, expected, steps_before) in cases {
        let (rows, steps_after) = read_page(&conn, &sql, before, ranged);
        assert_eq!(serde_json::to_value(&rows).unwrap(), expected);
        assert!(steps_before > 0);
        assert_eq!(
            steps_after, steps_before,
            "unrelated memberships increased pair seek work"
        );
        let legacy = sql.replace(
            "community_members_account_channel_member_sample",
            "community_members_account_member_sample",
        );
        let (legacy_rows, legacy_steps) = read_page(&conn, &legacy, before, ranged);
        assert_eq!(serde_json::to_value(legacy_rows).unwrap(), expected);
        if before == i64::MAX {
            assert!(
                legacy_steps > steps_after * 10,
                "fixture must expose account-only filtering work"
            );
        }
        let plan = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map(
                params![
                    "alice",
                    "room",
                    ranged.then_some("2020-01-01T00:00:01.000Z"),
                    ranged.then_some("2020-01-01T00:00:02.000Z"),
                    before,
                    11
                ],
                |row| row.get::<_, String>(3),
            )
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(plan.iter().any(|line| line.contains(
            "SEARCH m USING INDEX community_members_account_channel_member_sample (account=? AND channel=? AND id<?)"
        )), "{plan:?}");
        assert!(
            plan.iter().all(|line| !line.contains("TEMP B-TREE")),
            "{plan:?}"
        );
    }
}

#[test]
fn later_channel_cursor_seeks_without_sorting_or_traversing_retained_history() {
    let mut conn = Connection::open_in_memory().unwrap();
    initialize(&mut conn).unwrap();
    insert_retained_community_rows(&mut conn, 2000, 3000);
    let sql = receipt_sql(false, true, false, false);
    let read_page = |conn: &Connection, before| {
        let mut statement = conn.prepare(&sql).unwrap();
        let rows = statement
            .query_map(
                params![
                    None::<String>,
                    "room",
                    None::<String>,
                    None::<String>,
                    before,
                    11
                ],
                receipt_row,
            )
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0);
        assert_eq!(statement.get_status(StatementStatus::Sort), 0);
        (rows, statement.get_status(StatementStatus::VmStep))
    };
    let (first, _) = read_page(&conn, i64::MAX);
    assert_eq!(first.len(), 11);
    // Ten receipts plus a lookahead row use the same exclusive cursor as the public API.
    let cursor = first[9].member_id;
    let (before_growth, steps_before) = read_page(&conn, cursor);
    assert_eq!(before_growth[0].member_id, first[10].member_id);
    let expected: Vec<i64> = (1..=11).map(|offset| cursor - 2 * offset).collect();
    assert_eq!(
        before_growth
            .iter()
            .map(|row| row.member_id)
            .collect::<Vec<_>>(),
        expected
    );

    // Grow history on both sides of the cursor, with interleaved unrelated channels.
    // No timestamp filter may silently remove this old retained history.
    insert_retained_community_rows(&mut conn, 1, 1999);
    insert_retained_community_rows(&mut conn, 3001, 30000);
    let (after_growth, steps_after) = read_page(&conn, cursor);
    assert_eq!(
        after_growth
            .iter()
            .map(|row| row.member_id)
            .collect::<Vec<_>>(),
        expected
    );
    assert!(after_growth
        .iter()
        .all(|row| row.channel == "room" && row.received_at_utc == "2020-01-01T00:00:01.000Z"));
    assert!(steps_before > 0);
    assert_eq!(
        steps_after, steps_before,
        "later page work grew with retained history"
    );
    let plan = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(
            params![
                None::<String>,
                "room",
                None::<String>,
                None::<String>,
                cursor,
                11
            ],
            |row| row.get::<_, String>(3),
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        plan.iter().any(|line| line.contains(
            "SEARCH m USING INDEX community_members_channel_member (channel=? AND id<?)"
        )),
        "{plan:?}"
    );
    assert!(
        plan.iter().all(|line| !line.contains("TEMP B-TREE")),
        "{plan:?}"
    );
    assert!(conn.execute(
        "INSERT INTO community_members(sample_id,channel,account,roles) VALUES(2000,'other','mismatched',32)",
        [],
    ).is_err());
}

#[tokio::test]
async fn initialization_failure_is_classified_as_community_initialization() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("future.sqlite3");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE community_schema_version(id INTEGER PRIMARY KEY,version INTEGER);INSERT INTO community_schema_version VALUES(1,4);").unwrap();
    drop(conn);
    let diagnostics = crate::diagnostics::Diagnostics::new(&path);
    assert!(Store::open_with_diagnostics(&path, diagnostics.clone())
        .await
        .is_err());
    let record = diagnostics.snapshot().first_failure.unwrap();
    assert_eq!(record.operation, Operation::CommunityInitialize);
    assert_eq!(record.phase, Phase::Initialization);
    assert!(!record.progress_interrupt_requested);
}
