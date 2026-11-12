//! Search suggestions from YouTube's public completion service.
//!
//! The request carries only the typed prefix and YouTube's fixed client
//! parameters: no cookies, session data or language. One host is allowed,
//! including every redirect, and the JSONP reply is capped and parsed into
//! bounded strings.

use crate::{
    youtube::{capped_body, search_text},
    CancellationToken, MediaError, Result, YoutubeClient,
};
use serde_json::Value;
use std::{collections::HashSet, time::Duration};
use url::Url;

const SUGGEST_URL: &str = "https://suggestqueries-clients6.youtube.com/complete/search";
const SUGGEST_HOST: &str = "suggestqueries-clients6.youtube.com";
/// YouTube's own web client parameters; `q` is added per request. Without a
/// browser user agent the service answers in Latin-1 unless UTF-8 is asked for.
const SUGGEST_PARAMS: [(&str, &str); 4] = [
    ("client", "youtube"),
    ("ds", "yt"),
    ("ie", "utf-8"),
    ("oe", "utf-8"),
];
/// Most suggestions returned.
const SUGGESTION_LIMIT: usize = 10;
/// Longest suggestion kept, in characters; real ones are a few words.
const MAX_SUGGESTION_CHARS: usize = 200;
/// Replies are about 2 KiB; anything near this cap is not a suggestion list.
const SUGGESTION_RESPONSE_LIMIT: usize = 64 * 1024;
/// Suggestions are typed-ahead feedback: a slow answer is a useless answer.
const SUGGESTION_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REDIRECTS: usize = 3;
/// The reply is JSONP: `window.google.ac.h([prefix, [[suggestion, …], …], …])`.
const JSONP_PREFIX: &str = "window.google.ac.h(";
const JSONP_SUFFIX: &str = ")";

impl YoutubeClient {
    /// At most ten completions of `prefix` in YouTube's order, trimmed,
    /// deduplicated and free of control characters, from one request to
    /// YouTube's suggestion service that sends nothing but the prefix. The
    /// reply is capped at 64 KiB and ten seconds and honours `cancel`.
    pub async fn suggestions(
        &self,
        prefix: &str,
        cancel: CancellationToken,
    ) -> Result<Vec<String>> {
        let prefix = search_text(prefix)?;
        if cancel.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        let client = self
            .options
            .client_with_policy(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS || !suggest_url(attempt.url()) {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))?;
        let url = Url::parse_with_params(
            SUGGEST_URL,
            SUGGEST_PARAMS.into_iter().chain([("q", prefix)]),
        )
        .map_err(|_| MediaError::InvalidInput("Escribe una búsqueda válida.".into()))?;
        let invalid = || MediaError::Youtube("Las sugerencias no entregaron datos válidos.".into());
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(MediaError::Cancelled),
            result = tokio::time::timeout(SUGGESTION_TIMEOUT, async {
                let response = client
                    .get(url)
                    .send()
                    .await
                    .map_err(crate::network_error)?
                    .error_for_status()
                    .map_err(crate::network_error)?;
                let body = capped_body(response, SUGGESTION_RESPONSE_LIMIT, invalid).await?;
                let text = String::from_utf8(body).map_err(|_| invalid())?;
                parse_suggestions(&text).ok_or_else(invalid)
            }) => result.map_err(|_| {
                MediaError::Network("Las sugerencias tardaron demasiado.".into())
            })?,
        }
    }
}

/// Only `https` on the suggestion host, without credentials or a custom port.
fn suggest_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(SUGGEST_HOST)
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
}

/// Suggestions of a JSONP or plain JSON reply; `None` when it has no list.
fn parse_suggestions(body: &str) -> Option<Vec<String>> {
    let body = body.trim();
    let json = body
        .strip_prefix(JSONP_PREFIX)
        .and_then(|call| call.strip_suffix(JSONP_SUFFIX))
        .unwrap_or(body);
    let reply: Value = serde_json::from_str(json).ok()?;
    let mut seen = HashSet::new();
    Some(
        reply
            .get(1)?
            .as_array()?
            .iter()
            .filter_map(|entry| entry.as_str().or_else(|| entry.get(0)?.as_str()))
            .map(str::trim)
            .filter(|text| {
                !text.is_empty()
                    && text.chars().count() <= MAX_SUGGESTION_CHARS
                    && !text.chars().any(char::is_control)
            })
            .filter(|text| seen.insert(*text))
            .take(SUGGESTION_LIMIT)
            .map(str::to_owned)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The JSONP shape of a real 2026-11-11 reply, with placeholder queries:
    /// the first entry carries entity metadata, later ones only flags.
    const REPLY: &str = r#"window.google.ac.h(["artist o",[["artist one",0,[512,433,47],{"zae":"/g/fixture","zaf":"Singer","zai":"https://encrypted-tbn2.gstatic.com/images?q=tbn:fixture","zao":"Artist One"}],["artist one live",0,[512,433]],["  artist one live  ",0,[512]],["artist one 2026",0,[512,433,131]]],{"k":1,"q":"fixture"}])"#;

    #[test]
    fn real_reply_shape_gives_trimmed_distinct_suggestions() {
        assert_eq!(
            parse_suggestions(REPLY).unwrap(),
            ["artist one", "artist one live", "artist one 2026"]
        );
        assert_eq!(
            parse_suggestions(r#"["q",["plain","json"]]"#).unwrap(),
            ["plain", "json"]
        );
    }

    #[test]
    fn bounds_and_malformed_replies_are_handled_without_panicking() {
        let many: Vec<String> = (0..SUGGESTION_LIMIT + 5)
            .map(|n| format!("[\"s{n}\",0]"))
            .collect();
        let reply = format!("{JSONP_PREFIX}[\"s\",[{}]]{JSONP_SUFFIX}", many.join(","));
        assert_eq!(parse_suggestions(&reply).unwrap().len(), SUGGESTION_LIMIT);
        let long = "x".repeat(MAX_SUGGESTION_CHARS + 1);
        let filtered = format!("[\"q\",[\"ok\",\"bad\\u0007\",\"{long}\",\"\",42,[7],null]]");
        assert_eq!(parse_suggestions(&filtered).unwrap(), ["ok"]);
        for broken in [
            "",
            "window.google.ac.h(",
            "[]",
            "[\"q\"]",
            "{\"a\":1}",
            "[\"q\",\"text\"]",
            "<html>",
        ] {
            assert_eq!(parse_suggestions(broken), None, "{broken}");
        }
    }

    #[test]
    fn requests_and_redirects_may_only_reach_the_suggestion_host() {
        assert!(suggest_url(&Url::parse(SUGGEST_URL).unwrap()));
        for bad in [
            "http://suggestqueries-clients6.youtube.com/complete/search",
            "https://suggestqueries-clients6.youtube.com.evil.test/",
            "https://www.youtube.com/",
            "https://user:pass@suggestqueries-clients6.youtube.com/",
            "https://suggestqueries-clients6.youtube.com:8443/",
        ] {
            assert!(!suggest_url(&Url::parse(bad).unwrap()), "{bad}");
        }
    }
}
