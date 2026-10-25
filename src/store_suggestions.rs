//! Bounded login completion from retained local names, independent of history derivation.
use super::{normalize, Store, StoreError};
use crate::diagnostics::Operation;
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SuggestionKind {
    Account,
    Channel,
}

// Both statements seek through existing binary-collated indexes. Every valid login byte
// is below '{', so this range includes exactly the normalized prefix and its suffixes.
const ACCOUNT_SQL: &str = "SELECT DISTINCT account FROM relationships WHERE account>=?1 AND account<?2 ORDER BY account LIMIT ?3";
const CHANNEL_SQL: &str =
    "SELECT login FROM channels WHERE login>=?1 AND login<?2 ORDER BY login LIMIT ?3";

impl Store {
    pub async fn suggestions(
        &self,
        kind: SuggestionKind,
        prefix: String,
        limit: usize,
    ) -> Result<Vec<String>, StoreError> {
        if !(1..=10).contains(&limit) {
            return Err(StoreError::Invalid);
        }
        if prefix.trim().is_empty() {
            return Ok(Vec::new());
        }
        let prefix = normalize(&prefix)?;
        let upper = format!("{prefix}{{");
        self.call_read(Operation::Query, move |conn| {
            let sql = match kind {
                SuggestionKind::Account => ACCOUNT_SQL,
                SuggestionKind::Channel => CHANNEL_SQL,
            };
            let mut stmt = conn.prepare_cached(sql)?;
            let rows = stmt.query_map(params![prefix, upper, limit as i64], |row| row.get(0))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }
}

#[cfg(test)]
#[path = "store_suggestions_tests.rs"]
mod tests;
