//! Provider lookups through the profile's provider cache: one freshness
//! policy for every online list and page, and typed JSON entries that are
//! validated again when read back, because a database row is outside input.
//! See `docs/systems/Discovery.md`.

use crate::{discovery::unix_now, state::AppState};
use serde::{de::DeserializeOwned, Serialize};
use std::{future::Future, time::Duration};

/// A cached value younger than `ttl` (`fetched_at` in Unix seconds, compared
/// with `now`), else a fetched one handed to `store`, else the stale cached
/// value: offline keeps showing what was known. Errors only when nothing was
/// cached and the fetch failed.
pub(crate) async fn fresh_or_fetch<T>(
    cached: Option<(T, f64)>,
    ttl: Duration,
    now: f64,
    fetch: impl Future<Output = Result<T, String>>,
    store: impl FnOnce(&T),
) -> Result<T, String> {
    let stale = match cached {
        Some((value, fetched_at)) if now - fetched_at < ttl.as_secs_f64() => return Ok(value),
        other => other.map(|(value, _)| value),
    };
    match fetch.await {
        Ok(value) => {
            store(&value);
            Ok(value)
        }
        Err(error) => stale.ok_or(error),
    }
}

impl AppState {
    /// `T` cached under `key` with the policy of [`fresh_or_fetch`]. A cached
    /// entry that does not parse or fails `valid` counts as absent. Reads and
    /// writes run off the UI thread.
    pub(crate) async fn cached_json<T>(
        &self,
        key: String,
        ttl: Duration,
        valid: fn(&T) -> bool,
        fetch: impl Future<Output = Result<T, String>>,
    ) -> Result<T, String>
    where
        T: Serialize + DeserializeOwned + Send + 'static,
    {
        let core = self.services.core.clone();
        let read_key = key.clone();
        let cached = tokio::task::spawn_blocking(move || {
            let payload = core.cached_payload(&read_key).ok().flatten()?;
            let value: T = serde_json::from_str(&payload.json).ok()?;
            valid(&value).then_some((value, payload.fetched_at))
        })
        .await
        .ok()
        .flatten();
        let core = self.services.core.clone();
        fresh_or_fetch(cached, ttl, unix_now(), fetch, move |value| {
            if let Ok(json) = serde_json::to_string(value) {
                tokio::task::spawn_blocking(move || core.store_payload(&key, &json));
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: Duration = Duration::from_secs(60);

    #[tokio::test]
    async fn fresh_values_skip_the_fetch_and_stale_ones_survive_failures() {
        let fresh = fresh_or_fetch(
            Some(("cached", 100.0)),
            TTL,
            130.0,
            async { Ok("fetched") },
            |_| panic!("a fresh value is not stored again"),
        )
        .await;
        assert_eq!(fresh, Ok("cached"));
        let mut stored = None;
        let refreshed = fresh_or_fetch(
            Some(("cached", 0.0)),
            TTL,
            130.0,
            async { Ok("fetched") },
            |value| stored = Some(*value),
        )
        .await;
        assert_eq!((refreshed, stored), (Ok("fetched"), Some("fetched")));
        let offline = fresh_or_fetch(
            Some(("cached", 0.0)),
            TTL,
            130.0,
            async { Err("offline".to_owned()) },
            |_| {},
        )
        .await;
        assert_eq!(offline, Ok("cached"));
        let nothing = fresh_or_fetch(
            None::<(&str, f64)>,
            TTL,
            130.0,
            async { Err("offline".to_owned()) },
            |_| {},
        )
        .await;
        assert_eq!(nothing, Err("offline".to_owned()));
    }

    #[tokio::test]
    async fn invalid_cached_entries_are_refetched() {
        let mut app = crate::test_support::TestApp::new();
        let core = app.services.core.clone();
        core.store_payload("test:numbers", "[1,2,-3]").unwrap();
        let state = app.run(|state| state.clone());
        let value = state
            .cached_json::<Vec<i64>>(
                "test:numbers".into(),
                TTL,
                |numbers| numbers.iter().all(|n| *n > 0),
                async { Ok(vec![4, 5]) },
            )
            .await;
        assert_eq!(value, Ok(vec![4, 5]));
    }
}
