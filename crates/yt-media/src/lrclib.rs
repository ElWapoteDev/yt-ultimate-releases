//! LRCLIB lyrics catalog client (<https://lrclib.net>). Requests name only
//! the song (title, artist, duration); they never carry cookies or account
//! data, and every request and redirect stays on the catalog's host.

use crate::{CancellationToken, ConnectionOptions, MediaError, Result};
use reqwest::{header, StatusCode};
use serde::Deserialize;
use std::{ops::RangeInclusive, time::Duration};
use url::Url;
use yt_core::{Lyrics, LyricsCandidate, LyricsQuery};

/// The only host requests and redirects may reach.
const HOST: &str = "lrclib.net";
/// Exact lookup by title, artist and duration.
const GET_URL: &str = "https://lrclib.net/api/get";
/// Search by title and artist.
const SEARCH_URL: &str = "https://lrclib.net/api/search";
/// Most redirects followed, all within `HOST`.
const MAX_REDIRECTS: usize = 3;
/// Total time allowed for one request, body included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Largest body accepted from an exact lookup (one record).
const GET_RESPONSE_LIMIT: usize = 512 * 1024;
/// Largest body accepted from a search (up to `LRCLIB_SEARCH_LIMIT` records,
/// each with its lyrics; a popular song measured 392 KB).
const SEARCH_RESPONSE_LIMIT: usize = 2 * 1024 * 1024;
/// Records one search returns at most.
pub const LRCLIB_SEARCH_LIMIT: usize = 20;
/// Durations the exact lookup accepts, in seconds.
const GET_DURATION_SECONDS: RangeInclusive<f64> = 1.0..=3_600.0;
/// Wait assumed when the catalog asks to slow down without saying how long.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(60);
/// Longest wait honoured from a `Retry-After` header.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(3_600);

/// Client of the LRCLIB catalog. Clones share one connection pool.
#[derive(Clone)]
pub struct LrclibClient {
    client: reqwest::Client,
    user_agent: header::HeaderValue,
}

impl std::fmt::Debug for LrclibClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LrclibClient").finish_non_exhaustive()
    }
}

/// One catalog record. Every field is optional so a changed or partial
/// record degrades to "not usable" instead of failing the response.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    track_name: Option<String>,
    artist_name: Option<String>,
    album_name: Option<String>,
    duration: Option<f64>,
    instrumental: Option<bool>,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
    /// A Lyricsfile document, with word timing when `has_word_sync`.
    lyricsfile: Option<String>,
    has_word_sync: Option<bool>,
}

impl Record {
    /// The record as a candidate. Its synced lyrics come from the lyricsfile
    /// when the record has word sync and the document yields word timing,
    /// otherwise from the LRC; the two can disagree, so they are never mixed.
    fn candidate(self) -> Option<LyricsCandidate> {
        let title = self.track_name.as_deref()?;
        let synced = self
            .lyricsfile
            .as_deref()
            .filter(|_| self.has_word_sync == Some(true))
            .and_then(Lyrics::from_lyricsfile)
            .filter(Lyrics::has_word_timing)
            .or_else(|| self.synced_lyrics.as_deref().and_then(Lyrics::parse));
        LyricsCandidate::from_record(
            title,
            self.artist_name.as_deref().unwrap_or_default(),
            self.album_name.as_deref().unwrap_or_default(),
            self.duration.unwrap_or_default(),
            self.instrumental.unwrap_or_default(),
            synced,
            self.plain_lyrics.as_deref().and_then(Lyrics::parse),
        )
    }
}

impl LrclibClient {
    /// A client that honours the timeout and proxy of `options`, never its
    /// cookies. `user_agent` names the application to the catalog, which
    /// LRCLIB asks every client to do («name version (homepage)»); it must be
    /// a valid header value.
    pub fn new(options: &ConnectionOptions, user_agent: &str) -> Result<Self> {
        let user_agent = header::HeaderValue::from_str(user_agent).map_err(|_| {
            MediaError::InvalidInput("El identificador del cliente de letras no es válido.".into())
        })?;
        let client = options.client_with_policy(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS || !catalog_url(attempt.url()) {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))?;
        Ok(Self { client, user_agent })
    }

    /// The record LRCLIB holds for exactly this title, artist and duration
    /// (it accepts ±2 s). The album is left out because LRCLIB would require
    /// it to match exactly. `Ok(None)` when the catalog has none, or when the
    /// query has no artist or no duration within 1–3600 s.
    pub async fn get(
        &self,
        query: &LyricsQuery,
        cancel: CancellationToken,
    ) -> Result<Option<LyricsCandidate>> {
        if query.artist().is_empty() || !GET_DURATION_SECONDS.contains(&query.duration()) {
            return Ok(None);
        }
        let duration = format!("{:.0}", query.duration());
        let url = Url::parse_with_params(
            GET_URL,
            [
                ("track_name", query.title()),
                ("artist_name", query.artist()),
                ("duration", duration.as_str()),
            ],
        )
        .map_err(|_| MediaError::InvalidInput("La consulta de letras no es válida.".into()))?;
        let Some(body) = self.fetch(url, GET_RESPONSE_LIMIT, cancel).await? else {
            return Ok(None);
        };
        off_executor(move || {
            serde_json::from_slice::<Record>(&body)
                .ok()
                .and_then(Record::candidate)
        })
        .await
    }

    /// Records matching the query's title and artist, in catalog order, at
    /// most `LRCLIB_SEARCH_LIMIT`. Records that do not validate are dropped;
    /// a body that is not a list is an error.
    pub async fn search(
        &self,
        query: &LyricsQuery,
        cancel: CancellationToken,
    ) -> Result<Vec<LyricsCandidate>> {
        let mut pairs = vec![("track_name", query.title())];
        if !query.artist().is_empty() {
            pairs.push(("artist_name", query.artist()));
        }
        let url = Url::parse_with_params(SEARCH_URL, pairs)
            .map_err(|_| MediaError::InvalidInput("La consulta de letras no es válida.".into()))?;
        let body = self
            .fetch(url, SEARCH_RESPONSE_LIMIT, cancel)
            .await?
            .unwrap_or_default();
        off_executor(move || search_candidates(&body)).await?
    }

    /// GET `url` and return its body, at most `limit` bytes. `Ok(None)` for
    /// 404; 429 and 503 become `MediaError::Busy` with the requested wait.
    async fn fetch(
        &self,
        url: Url,
        limit: usize,
        cancel: CancellationToken,
    ) -> Result<Option<Vec<u8>>> {
        use futures_util::StreamExt;
        let request = async {
            let response = self
                .client
                .get(url)
                .header(header::USER_AGENT, self.user_agent.clone())
                .header(header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(crate::network_error)?;
            match response.status() {
                StatusCode::NOT_FOUND => return Ok(None),
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                    return Err(MediaError::Busy {
                        retry_after: retry_after(response.headers()),
                    });
                }
                _ => {}
            }
            let response = response.error_for_status().map_err(crate::network_error)?;
            if response
                .content_length()
                .is_some_and(|length| length > limit as u64)
            {
                return Err(MediaError::Network(
                    "la respuesta del catálogo de letras es demasiado grande.".into(),
                ));
            }
            let mut body = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(crate::network_error)?;
                if body.len() + chunk.len() > limit {
                    return Err(MediaError::Network(
                        "la respuesta del catálogo de letras es demasiado grande.".into(),
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(Some(body))
        };
        tokio::select! {
            _ = cancel.cancelled() => Err(MediaError::Cancelled),
            result = tokio::time::timeout(REQUEST_TIMEOUT, request) => result.map_err(|_| {
                MediaError::Network("el catálogo de letras tardó demasiado.".into())
            })?,
        }
    }
}

/// The candidates of a search body, in catalog order, at most
/// `LRCLIB_SEARCH_LIMIT`; a body that is not a list is an error.
fn search_candidates(body: &[u8]) -> Result<Vec<LyricsCandidate>> {
    let records: Vec<serde_json::Value> =
        serde_json::from_slice(body).map_err(|_| invalid_data())?;
    Ok(records
        .into_iter()
        .take(LRCLIB_SEARCH_LIMIT)
        .filter_map(|record| serde_json::from_value::<Record>(record).ok())
        .filter_map(Record::candidate)
        .collect())
}

/// Run `parse` on the blocking pool, so a body that takes longer than a frame
/// to parse (a search with word-synced records) never stalls the executor
/// that polls the lookup.
async fn off_executor<T: Send + 'static>(parse: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(parse)
        .await
        .map_err(|_| invalid_data())
}

fn invalid_data() -> MediaError {
    MediaError::Network("el catálogo de letras no entregó datos válidos.".into())
}

fn catalog_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(HOST)
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
}

/// The wait a 429 or 503 response asks for, in seconds; an absent or
/// unreadable header means `DEFAULT_RETRY_AFTER`, and the wait never exceeds
/// `MAX_RETRY_AFTER`.
fn retry_after(headers: &header::HeaderMap) -> Duration {
    headers
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map_or(DEFAULT_RETRY_AFTER, Duration::from_secs)
        .min(MAX_RETRY_AFTER)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEARCH_FIXTURE: &str = include_str!("../tests/fixtures/lrclib_search.json");

    #[test]
    fn only_the_catalog_host_over_https_is_reachable() {
        for (url, allowed) in [
            ("https://lrclib.net/api/search", true),
            ("http://lrclib.net/api/search", false),
            ("https://lrclib.net.evil.test/api", false),
            ("https://user@lrclib.net/api", false),
            ("https://lrclib.net:8443/api", false),
            ("https://evil.test/?lrclib.net", false),
        ] {
            assert_eq!(catalog_url(&Url::parse(url).unwrap()), allowed, "{url}");
        }
    }

    #[test]
    fn recorded_search_records_become_validated_candidates() {
        let candidates = search_candidates(SEARCH_FIXTURE.as_bytes()).unwrap();
        assert_eq!(candidates.len(), 5);
        let first = &candidates[0];
        assert_eq!(first.title, "Example Song");
        assert_eq!(first.artist, "Example Artist");
        assert_eq!(first.duration, 244.0);
        assert!(first
            .synced
            .as_ref()
            .is_some_and(|lyrics| lyrics.is_synced()));
        assert_eq!(
            first.synced.as_ref().unwrap().lines()[0].text,
            "First synced line"
        );
        assert!(!first.synced.as_ref().unwrap().has_word_timing());
        assert!(candidates[1].synced.is_none());
        assert!(candidates[1].plain.is_some());
        assert!(candidates[2].instrumental);
        let word_synced = candidates[3].synced.as_ref().unwrap();
        assert!(word_synced.has_word_timing());
        assert_eq!(word_synced.lines()[0].start_ms, 8_700);
        assert_eq!(word_synced.lines()[1].words[1].start_ms, 11_500);
        assert_eq!(word_synced.lines()[1].end_ms, Some(12_500));
        let other_version = candidates[4].synced.as_ref().unwrap();
        assert!(!other_version.has_word_timing());
        assert_eq!(other_version.lines()[0].start_ms, 8_700);
    }

    #[test]
    fn busy_responses_honour_a_bounded_retry_after() {
        let mut headers = header::HeaderMap::new();
        assert_eq!(retry_after(&headers), DEFAULT_RETRY_AFTER);
        headers.insert(header::RETRY_AFTER, header::HeaderValue::from_static("12"));
        assert_eq!(retry_after(&headers), Duration::from_secs(12));
        headers.insert(
            header::RETRY_AFTER,
            header::HeaderValue::from_static("999999"),
        );
        assert_eq!(retry_after(&headers), MAX_RETRY_AFTER);
        headers.insert(
            header::RETRY_AFTER,
            header::HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(retry_after(&headers), DEFAULT_RETRY_AFTER);
    }

    #[tokio::test]
    async fn lookups_without_artist_or_duration_make_no_request() {
        let client = LrclibClient::new(&ConnectionOptions::default(), "Test/1.0").unwrap();
        let query = LyricsQuery::new("Song", "", "", 200.0).unwrap();
        assert!(client
            .get(&query, CancellationToken::new())
            .await
            .unwrap()
            .is_none());
        let query = LyricsQuery::new("Song", "Artist", "", 0.0).unwrap();
        assert!(client
            .get(&query, CancellationToken::new())
            .await
            .unwrap()
            .is_none());
        assert!(LrclibClient::new(&ConnectionOptions::default(), "bad\nagent").is_err());
    }
}
