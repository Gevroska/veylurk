//! Bounded, read-only input extraction for the conditional Community model.
use crate::{
    diagnostics::Operation,
    presence_model::*,
    store::{normalize, Store, StoreError},
    timeline::TimelineQuery,
};
use rusqlite::{params, Connection};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

const SUCCESS_SQL:&str="SELECT id,returned_accounts FROM community_samples INDEXED BY community_samples_channel_receipt WHERE channel=?1 AND received_at_utc>=?2 AND received_at_utc<?3 AND outcome='success' ORDER BY received_at_utc,id LIMIT ?4";
const ATTEMPT_SQL:&str="SELECT s.id,s.run_id,s.outcome,s.started_at_utc,s.completed_at_utc,s.received_at_utc,s.reported_chatters,s.returned_accounts,s.broadcasters,s.staff,s.moderators,s.vips,s.chatbots,s.viewers,s.request_after_trigger_verified,s.request_after_trigger_ms,s.request_to_response_ms,s.request_to_finished_ms,s.finished_minus_response_ms,s.from_disk_cache,s.from_service_worker,s.from_prefetch_cache,s.served_from_cache_event,s.encoded_bytes,s.transport,s.http_status,m.roles FROM community_samples s NOT INDEXED LEFT JOIN community_members m ON m.sample_id=s.id AND m.account=?4 WHERE s.id>=?1 AND s.id<=?2 AND s.channel=?3 ORDER BY s.id";
const MEMBER_COUNTS_SQL:&str="SELECT COUNT(*),COALESCE(SUM(roles=32),0) FROM community_members INDEXED BY community_members_sample_member WHERE sample_id=?1";

impl Store {
    pub async fn community_presence(
        &self,
        query: PresenceModelQuery,
    ) -> Result<PresenceModelResponse, StoreError> {
        let parameters = query.parameters()?;
        let account = normalize(&query.account)?;
        if query.channel.is_none() && query.broadcast_id.is_none() {
            return Err(StoreError::Invalid);
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancellation = CancellationGuard(cancelled.clone());
        // This existing channel-only path resolves the exact shared viewport without IRC scans.
        let axis = self
            .timeline(TimelineQuery {
                account: None,
                channel: query.channel,
                broadcast_id: query.broadcast_id,
                from: query.from,
                to: query.to,
                limit: Some(1),
                before: None,
            })
            .await?;
        let response = PresenceModelResponse::new(account, axis, parameters)?;
        let Some(permit) = try_permit() else {
            return Ok(response.incomplete(PresenceStatus::AnalysisBusy));
        };
        let channel = response.channel.clone();
        let account = response.account.clone();
        let range = response.context_range.clone();
        let query_cancelled = cancelled.clone();
        let query_permit = permit.clone();
        let input = self
            .call_read(Operation::Query, move |conn| {
                // Owned by the actual queued/executing job even if the HTTP future is dropped.
                let _permit = query_permit;
                extract(
                    conn,
                    &channel,
                    &account,
                    &range.from,
                    &range.to,
                    &query_cancelled,
                )
            })
            .await?;
        let control = Control {
            cancelled,
            deadline: Instant::now() + Duration::from_secs(2),
        };
        tokio::task::spawn_blocking(move || {
            // Dropping/aborting a JoinHandle cannot stop blocking code. Cooperative checks
            // stop this bounded computation; the admission gate stays held until it exits.
            let _permit = permit;
            analyze(response, input, control)
        })
        .await
        .map_err(|_| StoreError::Unavailable)
    }
}

fn extract(
    conn: &mut Connection,
    channel: &str,
    account: &str,
    from: &str,
    to: &str,
    cancelled: &AtomicBool,
) -> Result<Extraction, StoreError> {
    let mut output = Extraction::default();
    if cancelled.load(Ordering::Acquire) {
        output.cancelled = true;
        return Ok(output);
    }
    let tx = conn.transaction()?;
    let mut low = i64::MAX;
    let mut high = 0_i64;
    let mut members = 0_i64;
    {
        let mut statement = tx.prepare(SUCCESS_SQL)?;
        let mut rows = statement.query(params![channel, from, to, (MAX_SAMPLES + 1) as i64])?;
        while let Some(row) = rows.next()? {
            if cancelled.load(Ordering::Acquire) {
                output.cancelled = true;
                return Ok(output);
            }
            output.successful += 1;
            let id: i64 = row.get(0)?;
            let count: Option<i64> = row.get(1)?;
            low = low.min(id);
            high = high.max(id);
            // Invalid counts are ineligible; reserve the per-response maximum for safety.
            let count = count
                .filter(|count| (0..=4096).contains(count))
                .unwrap_or(4096);
            members = members.saturating_add(count);
            if output.successful > MAX_SAMPLES || members > MAX_MEMBERS {
                output.limit = true;
                return Ok(output);
            }
        }
    }
    if output.successful == 0 {
        return Ok(output);
    }
    if high
        .checked_sub(low)
        .and_then(|span| span.checked_add(1))
        .is_none_or(|span| span > MAX_ID_SPAN)
    {
        output.limit = true;
        return Ok(output);
    }
    let from_time = parse_time(from)?;
    let to_time = parse_time(to)?;
    {
        let mut statement = tx.prepare(ATTEMPT_SQL)?;
        let mut count_statement = tx.prepare(MEMBER_COUNTS_SQL)?;
        let mut rows = statement.query(params![low, high, channel, account])?;
        let mut counted_members = 0_i64;
        while let Some(row) = rows.next()? {
            if cancelled.load(Ordering::Acquire) {
                output.cancelled = true;
                output.samples.clear();
                return Ok(output);
            }
            let mut sample = Sample {
                id: row.get(0)?,
                run_id: row.get(1)?,
                outcome: row.get(2)?,
                started: row.get(3)?,
                completed: row.get(4)?,
                received: row.get(5)?,
                reported: row.get(6)?,
                returned: row.get(7)?,
                categories: [
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                ],
                post_trigger: row.get(14)?,
                timing: [row.get(15)?, row.get(16)?, row.get(17)?, row.get(18)?],
                cache: [row.get(19)?, row.get(20)?, row.get(21)?, row.get(22)?],
                encoded: row.get(23)?,
                transport: row.get(24)?,
                http_status: row.get(25)?,
                target_roles: row.get(26)?,
                unique_members: 0,
                viewer_only: 0,
            };
            if sample.outcome == "success"
                && sample
                    .received
                    .as_deref()
                    .and_then(|at| parse_time(at).ok())
                    .is_some_and(|at| at >= from_time && at < to_time)
            {
                let (unique, viewers): (i64, i64) =
                    count_statement.query_row([sample.id], |r| Ok((r.get(0)?, r.get(1)?)))?;
                counted_members = counted_members.saturating_add(unique);
                if counted_members > MAX_MEMBERS {
                    output.limit = true;
                    output.samples.clear();
                    return Ok(output);
                }
                sample.unique_members = unique;
                sample.viewer_only = viewers;
            }
            output.samples.push(sample);
        }
    }
    tx.commit()?;
    Ok(output)
}

#[cfg(test)]
#[path = "store_presence_model_tests.rs"]
mod tests;
