use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};

pub const COMMUNITY_TAB_HASH: &str =
    "92168b4434c8f4d32df14510052131c3544b929723d5f8b69bb96c96207e483e";
pub const TWITCH_WEB_CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeError {
    #[error("Twitch required a browser integrity challenge; standalone anonymous collection is unavailable")]
    IntegrityChallenge,
    #[error("request was unauthorized")]
    Unauthorized,
    #[error("request was rate limited")]
    RateLimited,
    #[error("GraphQL rejected the persisted query")]
    PersistedQueryRejected,
    #[error("GraphQL error: {0}")]
    GraphQl(String),
    #[error("response schema is incompatible: {0}")]
    Schema(String),
    #[error("response channel {actual:?} did not match requested channel {requested:?}")]
    ChannelMismatch { requested: String, actual: String },
}

#[derive(Debug, Deserialize)]
struct Envelope {
    data: Option<Data>,
    #[serde(default)]
    errors: Vec<GraphError>,
    extensions: Option<Extensions>,
}

#[derive(Debug, Deserialize)]
struct Data {
    user: Option<User>,
}

#[derive(Debug, Deserialize)]
struct User {
    id: Option<String>,
    channel: Option<Channel>,
    stream: Option<Stream>,
}

#[derive(Debug, Deserialize)]
struct Stream {
    id: String,
    title: String,
}

#[derive(Debug, Deserialize)]
struct Channel {
    id: Option<String>,
    name: String,
    chatters: Option<Chatters>,
}

#[derive(Debug, Deserialize)]
struct Chatters {
    count: u64,
    broadcasters: Option<Vec<Account>>,
    staff: Option<Vec<Account>>,
    moderators: Option<Vec<Account>>,
    vips: Option<Vec<Account>>,
    chatbots: Option<Vec<Account>>,
    viewers: Option<Vec<Account>>,
}

#[derive(Debug, Deserialize)]
struct Account {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GraphError {
    message: String,
    path: Option<Vec<String>>,
    extensions: Option<ErrorExtensions>,
}

#[derive(Debug, Deserialize)]
struct ErrorExtensions {
    code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Extensions {
    challenge: Option<Challenge>,
}

#[derive(Debug, Deserialize)]
struct Challenge {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Clone)]
pub struct ParsedSample {
    pub channel_id: Option<String>,
    pub channel_login: String,
    pub stream_id: Option<String>,
    pub stream_title: Option<String>,
    pub reported_count: u64,
    pub unique_logins: HashSet<String>,
    pub role_rows: BTreeMap<&'static str, usize>,
    pub fingerprint: String,
}

pub fn request_body(login: &str) -> serde_json::Value {
    serde_json::json!([{
        "operationName": "CommunityTab",
        "variables": { "login": login },
        "extensions": { "persistedQuery": { "version": 1, "sha256Hash": COMMUNITY_TAB_HASH } }
    }])
}

pub fn parse_response(requested: &str, bytes: &[u8]) -> Result<ParsedSample, ProbeError> {
    let envelopes: Vec<Envelope> = serde_json::from_slice(bytes)
        .map_err(|error| ProbeError::Schema(format!("invalid JSON: {error}")))?;
    if envelopes.len() != 1 {
        return Err(ProbeError::Schema(format!(
            "expected one response envelope, received {}",
            envelopes.len()
        )));
    }
    let envelope = envelopes.into_iter().next().expect("length checked");

    if envelope
        .extensions
        .as_ref()
        .and_then(|e| e.challenge.as_ref())
        .is_some_and(|c| c.kind.eq_ignore_ascii_case("integrity"))
        || envelope.errors.iter().any(|e| {
            e.extensions.as_ref().and_then(|x| x.code.as_deref()) == Some("IntegrityCheckFailed")
        })
    {
        return Err(ProbeError::IntegrityChallenge);
    }
    if let Some(error) = envelope.errors.first() {
        let code = error.extensions.as_ref().and_then(|x| x.code.as_deref());
        return Err(match code {
            Some("Unauthorized") => ProbeError::Unauthorized,
            Some("RateLimitExceeded" | "TooManyRequests") => ProbeError::RateLimited,
            Some("PersistedQueryNotFound" | "PersistedQueryNotSupported") => {
                ProbeError::PersistedQueryRejected
            }
            _ => ProbeError::GraphQl(match &error.path {
                Some(path) => format!("{} at {}", error.message, path.join(".")),
                None => error.message.clone(),
            }),
        });
    }

    let user = envelope
        .data
        .and_then(|d| d.user)
        .ok_or_else(|| ProbeError::Schema("missing data.user".into()))?;
    let channel = user
        .channel
        .ok_or_else(|| ProbeError::Schema("missing data.user.channel".into()))?;
    if !channel.name.eq_ignore_ascii_case(requested) {
        return Err(ProbeError::ChannelMismatch {
            requested: requested.to_owned(),
            actual: channel.name,
        });
    }
    if let (Some(user_id), Some(channel_id)) = (&user.id, &channel.id) {
        if user_id != channel_id {
            return Err(ProbeError::Schema("user and channel IDs differ".into()));
        }
    }
    let chatters = channel
        .chatters
        .ok_or_else(|| ProbeError::Schema("missing channel.chatters".into()))?;
    let required = |value: Option<Vec<Account>>, name: &str| {
        value.ok_or_else(|| ProbeError::Schema(format!("missing chatters.{name}")))
    };
    let groups = [
        (
            "broadcasters",
            required(chatters.broadcasters, "broadcasters")?,
        ),
        ("staff", required(chatters.staff, "staff")?),
        ("moderators", required(chatters.moderators, "moderators")?),
        ("vips", required(chatters.vips, "vips")?),
        ("chatbots", required(chatters.chatbots, "chatbots")?),
        ("viewers", required(chatters.viewers, "viewers")?),
    ];
    let mut unique_logins = HashSet::new();
    let mut role_rows = BTreeMap::new();
    for (role, accounts) in groups {
        role_rows.insert(role, accounts.len());
        for account in accounts {
            if account.login.trim().is_empty() {
                return Err(ProbeError::Schema("empty account login".into()));
            }
            unique_logins.insert(account.login.to_ascii_lowercase());
        }
    }
    let mut normalized: Vec<_> = unique_logins.iter().cloned().collect();
    normalized.sort_unstable();
    let fingerprint = format!("{:x}", Sha256::digest(normalized.join("\n").as_bytes()));
    let (stream_id, stream_title) = user
        .stream
        .map(|s| (Some(s.id), Some(s.title)))
        .unwrap_or((None, None));
    Ok(ParsedSample {
        channel_id: channel.id,
        channel_login: requested.to_ascii_lowercase(),
        stream_id,
        stream_title,
        reported_count: chatters.count,
        unique_logins,
        role_rows,
        fingerprint,
    })
}

pub fn overlap(left: &HashSet<String>, right: &HashSet<String>) -> usize {
    left.intersection(right).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(name: &str) -> String {
        format!(
            r#"[{{"data":{{"user":{{"id":"7","channel":{{"id":"7","name":"{name}","chatters":{{"count":3,"broadcasters":[{{"login":"Owner"}}],"staff":[],"moderators":[{{"login":"DUP"}}],"vips":[],"chatbots":[],"viewers":[{{"login":"dup"}},{{"login":"Alice"}}]}}}},"stream":{{"id":"9","title":"Live"}}}}}}}}]"#
        )
    }

    #[test]
    fn parses_and_deduplicates_roles_case_insensitively() {
        let sample = parse_response("mixedcase", valid("MixedCase").as_bytes()).unwrap();
        assert_eq!(sample.unique_logins.len(), 3);
        assert_eq!(sample.role_rows["viewers"], 2);
        assert_eq!(sample.stream_id.as_deref(), Some("9"));
    }

    #[test]
    fn rejects_cross_channel_response() {
        assert!(matches!(
            parse_response("expected", valid("other").as_bytes()),
            Err(ProbeError::ChannelMismatch { .. })
        ));
    }

    #[test]
    fn classifies_integrity_challenge_even_with_partial_data() {
        let body = br#"[{"errors":[{"message":"failed integrity check","extensions":{"code":"IntegrityCheckFailed"}}],"data":{"user":null},"extensions":{"challenge":{"type":"integrity"}}}]"#;
        assert!(matches!(
            parse_response("x", body),
            Err(ProbeError::IntegrityChallenge)
        ));
    }

    #[test]
    fn classifies_invalid_json_as_schema_failure() {
        assert!(matches!(
            parse_response("x", b"not-json"),
            Err(ProbeError::Schema(_))
        ));
    }

    #[test]
    fn rejects_missing_viewers_field() {
        let body = valid("x").replace(
            r#""viewers":[{"login":"dup"},{"login":"Alice"}]"#,
            r#""unexpected":[]"#,
        );
        assert!(matches!(
            parse_response("x", body.as_bytes()),
            Err(ProbeError::Schema(_))
        ));
    }

    #[test]
    fn rejects_multiple_response_envelopes() {
        let one = valid("x");
        let inner = &one[1..one.len() - 1];
        let body = format!("[{inner},{inner}]");
        assert!(matches!(
            parse_response("x", body.as_bytes()),
            Err(ProbeError::Schema(_))
        ));
    }

    #[test]
    fn computes_cross_sample_overlap() {
        let a = HashSet::from(["a".into(), "b".into()]);
        let b = HashSet::from(["b".into(), "c".into()]);
        assert_eq!(overlap(&a, &b), 1);
    }
}
