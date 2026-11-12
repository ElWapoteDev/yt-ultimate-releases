//! Searches the user ran, for suggestions and the recent-searches list.

use crate::{AppCore, CoreError, Result, now};
use rusqlite::params;

/// Searches kept; the least recently used are pruned on write.
pub const SEARCH_HISTORY_LIMIT: usize = 300;
/// Longest accepted search, in characters.
pub const MAX_SEARCH_CHARS: usize = 200;

/// One remembered search.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHistoryEntry {
    /// As the user last typed it, trimmed.
    pub query: String,
    pub uses: u32,
    /// Unix seconds.
    pub first_at: f64,
    pub last_at: f64,
}

/// Case-insensitive identity of a search with collapsed whitespace.
fn query_key(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn validate(query: &str) -> Result<String> {
    let query = query.split_whitespace().collect::<Vec<_>>().join(" ");
    if query.is_empty()
        || query.chars().count() > MAX_SEARCH_CHARS
        || query.chars().any(char::is_control)
    {
        return Err(CoreError::Invalid("Invalid search".into()));
    }
    Ok(query)
}

impl AppCore {
    /// Remember a search the user ran.
    pub fn record_search(&self, query: &str) -> Result<()> {
        let query = validate(query)?;
        let mut db = self.connection()?;
        let transaction = db.transaction()?;
        transaction.execute(
            "INSERT INTO rust_search_history(query_key,query,uses,first_at,last_at) VALUES(?1,?2,1,?3,?3)
             ON CONFLICT(query_key) DO UPDATE SET query=excluded.query,uses=uses+1,last_at=excluded.last_at",
            params![query_key(&query), query, now()],
        )?;
        transaction.execute(
            "DELETE FROM rust_search_history WHERE query_key IN (
                SELECT query_key FROM rust_search_history ORDER BY last_at DESC LIMIT -1 OFFSET ?1)",
            [SEARCH_HISTORY_LIMIT as i64],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Remembered searches, most recent first.
    pub fn search_history(&self, limit: usize) -> Result<Vec<SearchHistoryEntry>> {
        let db = self.connection()?;
        let mut query = db.prepare(
            "SELECT query,uses,first_at,last_at FROM rust_search_history ORDER BY last_at DESC LIMIT ?1",
        )?;
        let rows = query.query_map([limit.min(SEARCH_HISTORY_LIMIT) as i64], |row| {
            Ok(SearchHistoryEntry {
                query: row.get(0)?,
                uses: row.get::<_, i64>(1)?.clamp(1, i64::from(u32::MAX)) as u32,
                first_at: row.get(2)?,
                last_at: row.get(3)?,
            })
        })?;
        Ok(rows
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|entry| validate(&entry.query).is_ok())
            .collect())
    }

    /// Forget one search.
    pub fn forget_search(&self, query: &str) -> Result<()> {
        self.connection()?.execute(
            "DELETE FROM rust_search_history WHERE query_key=?1",
            [query_key(query)],
        )?;
        Ok(())
    }

    /// Forget every search; returns how many were removed.
    pub fn clear_search_history(&self) -> Result<usize> {
        Ok(self
            .connection()?
            .execute("DELETE FROM rust_search_history", [])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_merge_by_case_and_spacing_and_stay_bounded() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        core.record_search("Peso  Pluma").unwrap();
        core.record_search("peso pluma").unwrap();
        core.record_search("Bad Bunny").unwrap();
        let history = core.search_history(10).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].query, "Bad Bunny");
        assert_eq!(history[1].query, "peso pluma");
        assert_eq!(history[1].uses, 2);
        core.forget_search("PESO PLUMA").unwrap();
        assert_eq!(core.search_history(10).unwrap().len(), 1);
        core.record_search("line\nbreak").unwrap();
        assert_eq!(core.search_history(1).unwrap()[0].query, "line break");
        for invalid in ["", "   ", "bell\u{7}"] {
            assert!(core.record_search(invalid).is_err());
        }
        assert!(
            core.record_search(&"x".repeat(MAX_SEARCH_CHARS + 1))
                .is_err()
        );
        for index in 0..=SEARCH_HISTORY_LIMIT {
            core.record_search(&format!("query {index}")).unwrap();
        }
        assert_eq!(
            core.search_history(usize::MAX).unwrap().len(),
            SEARCH_HISTORY_LIMIT
        );
        assert_eq!(core.clear_search_history().unwrap(), SEARCH_HISTORY_LIMIT);
    }
}
