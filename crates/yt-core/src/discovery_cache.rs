//! Bounded cache of provider responses (recommendation lists, artist and
//! album pages), keyed by the request that produced them. Freshness is the
//! caller's policy; the store keeps the time. Payloads are untrusted when read
//! back: typed readers validate them again.

use crate::{AppCore, CoreError, RemoteMedia, Result, now};
use rusqlite::{OptionalExtension, params};

/// Most items stored in one cached recommendation list.
pub const DISCOVERY_LIST_LIMIT: usize = 100;
/// Cached responses kept; the least recently fetched are pruned on write.
pub const DISCOVERY_CACHE_LIMIT: usize = 400;
/// Largest cached payload, in bytes.
pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
/// Longest accepted cache key, in bytes.
const MAX_KEY_BYTES: usize = 256;
/// Namespace of recommendation lists inside the cache.
const LIST_PREFIX: &str = "list:";

/// A cached list and when it was fetched (Unix seconds).
#[derive(Debug, Clone, PartialEq)]
pub struct CachedDiscovery {
    pub items: Vec<RemoteMedia>,
    pub fetched_at: f64,
}

/// A cached raw payload and when it was fetched (Unix seconds).
#[derive(Debug, Clone, PartialEq)]
pub struct CachedPayload {
    pub json: String,
    pub fetched_at: f64,
}

fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES || key.chars().any(char::is_control) {
        return Err(CoreError::Invalid("Invalid cache key".into()));
    }
    Ok(())
}

impl AppCore {
    /// The cached payload for `key`, if any. The caller parses and validates it.
    pub fn cached_payload(&self, key: &str) -> Result<Option<CachedPayload>> {
        validate_key(key)?;
        Ok(self
            .connection()?
            .query_row(
                "SELECT payload_json,fetched_at FROM rust_provider_cache WHERE key=?1",
                [key],
                |row| {
                    Ok(CachedPayload {
                        json: row.get(0)?,
                        fetched_at: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// Replace the payload for `key`; it must be JSON within
    /// `MAX_PAYLOAD_BYTES`. The cache is then pruned to its limit.
    pub fn store_payload(&self, key: &str, json: &str) -> Result<()> {
        validate_key(key)?;
        if json.len() > MAX_PAYLOAD_BYTES {
            return Err(CoreError::Invalid("Cached payload is too large".into()));
        }
        serde_json::from_str::<serde_json::Value>(json)?;
        let mut db = self.connection()?;
        let transaction = db.transaction()?;
        transaction.execute(
            "INSERT INTO rust_provider_cache(key,payload_json,fetched_at) VALUES(?1,?2,?3)
             ON CONFLICT(key) DO UPDATE SET payload_json=excluded.payload_json,fetched_at=excluded.fetched_at",
            params![key, json, now()],
        )?;
        transaction.execute(
            "DELETE FROM rust_provider_cache WHERE key IN (
                SELECT key FROM rust_provider_cache ORDER BY fetched_at DESC,key LIMIT -1 OFFSET ?1)",
            [DISCOVERY_CACHE_LIMIT as i64],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// The cached recommendation list for `key`. Items that no longer
    /// validate are dropped; an unreadable list reads as absent.
    pub fn cached_discovery(&self, key: &str) -> Result<Option<CachedDiscovery>> {
        validate_key(key)?;
        let Some(payload) = self.cached_payload(&format!("{LIST_PREFIX}{key}"))? else {
            return Ok(None);
        };
        Ok(serde_json::from_str::<Vec<RemoteMedia>>(&payload.json)
            .ok()
            .map(|items| CachedDiscovery {
                items: items
                    .into_iter()
                    .filter(|item| item.validate().is_ok())
                    .take(DISCOVERY_LIST_LIMIT)
                    .collect(),
                fetched_at: payload.fetched_at,
            }))
    }

    /// Replace the recommendation list for `key`. Every item must validate
    /// and the list must fit `DISCOVERY_LIST_LIMIT`.
    pub fn store_discovery(&self, key: &str, items: &[RemoteMedia]) -> Result<()> {
        validate_key(key)?;
        if items.len() > DISCOVERY_LIST_LIMIT {
            return Err(CoreError::Invalid("Discovery list is too long".into()));
        }
        for item in items {
            item.validate()?;
        }
        self.store_payload(
            &format!("{LIST_PREFIX}{key}"),
            &serde_json::to_string(items)?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(video: &str) -> RemoteMedia {
        RemoteMedia::from_parts(video, "Song", "Artist", "", 120.0).unwrap()
    }

    #[test]
    fn lists_round_trip_and_are_replaced_by_key() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        assert!(core.cached_discovery("mix:a").unwrap().is_none());
        core.store_discovery("mix:a", &[item("abcdefghijk")])
            .unwrap();
        core.store_discovery("mix:a", &[item("lmnopqrstuv"), item("abcdefghijk")])
            .unwrap();
        let cached = core.cached_discovery("mix:a").unwrap().unwrap();
        assert_eq!(cached.items.len(), 2);
        assert_eq!(cached.items[0].id, "youtube:lmnopqrstuv");
        assert!(cached.fetched_at > 0.0);
    }

    #[test]
    fn invalid_keys_items_payloads_and_corrupt_rows_never_escape() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        assert!(core.store_discovery("", &[]).is_err());
        assert!(core.store_discovery("bad\nkey", &[]).is_err());
        let mut invalid = item("abcdefghijk");
        invalid.title.clear();
        assert!(core.store_discovery("mix:b", &[invalid]).is_err());
        let too_many = (0..=DISCOVERY_LIST_LIMIT)
            .map(|_| item("abcdefghijk"))
            .collect::<Vec<_>>();
        assert!(core.store_discovery("mix:b", &too_many).is_err());
        assert!(core.store_payload("artist:x", "not json").is_err());
        let huge = format!("\"{}\"", "x".repeat(MAX_PAYLOAD_BYTES));
        assert!(core.store_payload("artist:x", &huge).is_err());
        core.connection()
            .unwrap()
            .execute(
                "INSERT INTO rust_provider_cache(key,payload_json,fetched_at) VALUES('list:mix:c','not json',1)",
                [],
            )
            .unwrap();
        assert!(core.cached_discovery("mix:c").unwrap().is_none());
        core.store_payload("artist:y", "{\"name\":\"A\"}").unwrap();
        assert_eq!(
            core.cached_payload("artist:y").unwrap().unwrap().json,
            "{\"name\":\"A\"}"
        );
    }

    #[test]
    fn cache_is_bounded() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        for index in 0..=DISCOVERY_CACHE_LIMIT {
            core.store_discovery(&format!("search:{index}"), &[])
                .unwrap();
        }
        let count: i64 = core
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM rust_provider_cache", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, DISCOVERY_CACHE_LIMIT as i64);
    }
}
