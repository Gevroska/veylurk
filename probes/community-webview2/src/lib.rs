//! Bounded parsing and aggregate evidence only. Account names never serialize.
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

pub const REQUEST_CAP: usize = 64 * 1024;
pub const RESPONSE_CAP: usize = 1024 * 1024;
pub const MAX_BATCH: usize = 32;
pub const MAX_SAMPLES: usize = 3;
pub const CATEGORIES: [&str; 6] = [
    "broadcasters",
    "staff",
    "moderators",
    "vips",
    "chatbots",
    "viewers",
];

pub fn channel_login(input: &str) -> Option<String> {
    let input = input.trim();
    (!input.is_empty()
        && input.len() <= 25
        && input
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_'))
    .then(|| input.to_ascii_lowercase())
}

/// The URI check never retains a query string, credentials, or headers.
pub fn is_source_uri(uri: &str) -> bool {
    uri == "https://gql.twitch.tv/gql" || uri.starts_with("https://gql.twitch.tv/gql?")
}

pub fn is_channel_page(uri: &str, channel: &str) -> bool {
    let Some(path) = uri.strip_prefix("https://www.twitch.tv/") else {
        return false;
    };
    let login = path.split(['/', '?', '#']).next().unwrap_or("");
    login.eq_ignore_ascii_case(channel)
        && !matches!(
            login.to_ascii_lowercase().as_str(),
            "login" | "signup" | "settings"
        )
}

#[derive(Clone, Debug)]
pub struct Selection {
    pub index: usize,
    pub batch_len: usize,
    pub batched: bool,
    pub operation: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    RequestTooLarge,
    ResponseTooLarge,
    RequestReadFailed,
    ResponseReadFailed,
    InvalidJson,
    BatchLimit,
    AmbiguousSource,
    BatchMismatch,
    HttpDenied,
    HttpError,
    IntegrityDenied,
    AuthenticationDenied,
    ChallengeDenied,
    SourceErrors,
    MissingChatters,
    InvalidShape,
}

impl Failure {
    pub fn code(self) -> &'static str {
        match self {
            Self::RequestTooLarge => "request_size_limit",
            Self::ResponseTooLarge => "response_size_limit",
            Self::RequestReadFailed => "request_stream_unavailable",
            Self::ResponseReadFailed => "response_stream_unavailable",
            Self::InvalidJson => "invalid_json",
            Self::BatchLimit => "batch_limit",
            Self::AmbiguousSource => "ambiguous_source",
            Self::BatchMismatch => "batch_mismatch",
            Self::HttpDenied => "http_denied",
            Self::HttpError => "http_error",
            Self::IntegrityDenied => "integrity_denied",
            Self::AuthenticationDenied => "authentication_denied",
            Self::ChallengeDenied => "challenge_denied",
            Self::SourceErrors => "source_errors",
            Self::MissingChatters => "missing_chatters",
            Self::InvalidShape => "invalid_shape",
        }
    }
    pub fn is_denial(self) -> bool {
        matches!(
            self,
            Self::HttpDenied
                | Self::IntegrityDenied
                | Self::AuthenticationDenied
                | Self::ChallengeDenied
        )
    }
}

fn bounded_json(bytes: &[u8], cap: usize, size_error: Failure) -> Result<Value, Failure> {
    if bytes.len() > cap {
        return Err(size_error);
    }
    serde_json::from_slice(bytes).map_err(|_| Failure::InvalidJson)
}

/// Select one exact native source operation for the armed channel, without
/// retaining any other operations, variables, or request data.
pub fn select_request(bytes: &[u8], channel: &str) -> Result<Option<Selection>, Failure> {
    let value = bounded_json(bytes, REQUEST_CAP, Failure::RequestTooLarge)?;
    let batched = value.is_array();
    let items: Vec<&Value> = match &value {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![&value],
        _ => return Err(Failure::InvalidShape),
    };
    if items.is_empty() || items.len() > MAX_BATCH {
        return Err(Failure::BatchLimit);
    }
    let mut selected = None;
    for (index, item) in items.iter().enumerate() {
        let operation = match item.get("operationName").and_then(Value::as_str) {
            Some("CommunityTab") => "CommunityTab",
            Some("ChatViewers") => "ChatViewers",
            _ => continue,
        };
        let variable = if operation == "CommunityTab" {
            "login"
        } else {
            "channelLogin"
        };
        let login = item
            .pointer(&format!("/variables/{variable}"))
            .and_then(Value::as_str);
        if !login.is_some_and(|login| login.eq_ignore_ascii_case(channel)) {
            continue;
        }
        if selected.is_some() {
            return Err(Failure::AmbiguousSource);
        }
        selected = Some(Selection {
            index,
            batch_len: items.len(),
            batched,
            operation,
        });
    }
    Ok(selected)
}

#[derive(Debug)]
pub struct ParsedSample {
    pub count: u64,
    pub categories: BTreeMap<String, usize>,
    pub accounts: HashSet<String>,
}

/// Error text is classified in memory and is never returned or serialized.
fn source_error(errors: &[Value]) -> Failure {
    for error in errors {
        let text = error.to_string().to_ascii_lowercase();
        if text.contains("integrity") {
            return Failure::IntegrityDenied;
        }
        if [
            "captcha",
            "challenge",
            "verification",
            "access denied",
            "forbidden",
        ]
        .iter()
        .any(|x| text.contains(x))
        {
            return Failure::ChallengeDenied;
        }
        if [
            "unauthorized",
            "unauthenticated",
            "authentication",
            "login required",
            "not authenticated",
            "invalid oauth",
        ]
        .iter()
        .any(|x| text.contains(x))
        {
            return Failure::AuthenticationDenied;
        }
    }
    Failure::SourceErrors
}

pub fn parse_response(
    bytes: &[u8],
    selection: &Selection,
    status: u16,
) -> Result<ParsedSample, Failure> {
    if matches!(status, 401 | 403 | 429) {
        return Err(Failure::HttpDenied);
    }
    if status != 200 {
        return Err(Failure::HttpError);
    }
    let value = bounded_json(bytes, RESPONSE_CAP, Failure::ResponseTooLarge)?;
    let item = if selection.batched {
        let items = value.as_array().ok_or(Failure::BatchMismatch)?;
        if items.len() != selection.batch_len {
            return Err(Failure::BatchMismatch);
        }
        items.get(selection.index).ok_or(Failure::BatchMismatch)?
    } else {
        if !value.is_object() {
            return Err(Failure::BatchMismatch);
        }
        &value
    };
    if let Some(errors) = item.get("errors") {
        let errors = errors.as_array().ok_or(Failure::InvalidShape)?;
        if !errors.is_empty() {
            return Err(source_error(errors));
        }
    }
    let chatters = item
        .pointer("/data/user/channel/chatters")
        .or_else(|| item.pointer("/data/channel/chatters"))
        .filter(|v| v.is_object())
        .ok_or(Failure::MissingChatters)?;
    let count = chatters
        .get("count")
        .and_then(Value::as_u64)
        .ok_or(Failure::InvalidShape)?;
    let mut categories = BTreeMap::new();
    let mut accounts = HashSet::new();
    for category in CATEGORIES {
        let entries = chatters
            .get(category)
            .and_then(Value::as_array)
            .ok_or(Failure::InvalidShape)?;
        categories.insert(category.to_owned(), entries.len());
        for entry in entries {
            let login = entry
                .get("login")
                .and_then(Value::as_str)
                .and_then(channel_login)
                .ok_or(Failure::InvalidShape)?;
            accounts.insert(login);
        }
    }
    Ok(ParsedSample {
        count,
        categories,
        accounts,
    })
}

#[derive(Debug, Serialize)]
pub struct SampleEvidence {
    pub sequence: usize,
    pub observed_unix_seconds: u64,
    pub operation: &'static str,
    pub http_status: u16,
    pub source_error_count: usize,
    pub count: u64,
    pub categories: BTreeMap<String, usize>,
    pub unique_accounts: usize,
    pub overlap_with_previous: Option<usize>,
    pub union_accounts: usize,
}

#[derive(Debug, Serialize)]
pub struct BrowserDiagnostics {
    pub requested_target: &'static str,
    pub native_action_count: u64,
    pub navigation_started_count: u64,
    pub navigation_completed_count: u64,
    pub last_navigation_id: Option<u64>,
    pub last_navigation_success: Option<bool>,
    pub last_web_error_status: Option<i32>,
    pub document_loaded_count: u64,
    pub process_failure_kind: Option<i32>,
}

impl Default for BrowserDiagnostics {
    fn default() -> Self {
        Self {
            requested_target: "none",
            native_action_count: 0,
            navigation_started_count: 0,
            navigation_completed_count: 0,
            last_navigation_id: None,
            last_navigation_success: None,
            last_web_error_status: None,
            document_loaded_count: 0,
            process_failure_kind: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Evidence {
    pub schema_version: u8,
    pub source: &'static str,
    pub phase: &'static str,
    pub status: &'static str,
    pub channel: Option<String>,
    pub source_proven_in_this_run: bool,
    pub sample_count: usize,
    pub manual_attempt_count: usize,
    pub source_errors: Vec<&'static str>,
    pub browser_cleanup: &'static str,
    pub samples: Vec<SampleEvidence>,
    pub browser_diagnostics: BrowserDiagnostics,
    #[serde(skip)]
    previous: HashSet<String>,
    #[serde(skip)]
    union: HashSet<String>,
}

impl Default for Evidence {
    fn default() -> Self {
        Self {
            schema_version: 1,
            source: "native_twitch_community_webview2",
            phase: "starting",
            status: "source_unproved",
            channel: None,
            source_proven_in_this_run: false,
            sample_count: 0,
            manual_attempt_count: 0,
            source_errors: Vec::new(),
            browser_cleanup: "not_started",
            samples: Vec::new(),
            browser_diagnostics: BrowserDiagnostics::default(),
            previous: HashSet::new(),
            union: HashSet::new(),
        }
    }
}

impl Evidence {
    pub fn add(&mut self, parsed: ParsedSample, selection: &Selection, timestamp: u64) -> bool {
        if self.samples.len() >= MAX_SAMPLES {
            return false;
        }
        let overlap = (!self.samples.is_empty())
            .then(|| self.previous.intersection(&parsed.accounts).count());
        self.union.extend(parsed.accounts.iter().cloned());
        self.samples.push(SampleEvidence {
            sequence: self.samples.len() + 1,
            observed_unix_seconds: timestamp,
            operation: selection.operation,
            http_status: 200,
            source_error_count: 0,
            count: parsed.count,
            categories: parsed.categories,
            unique_accounts: parsed.accounts.len(),
            overlap_with_previous: overlap,
            union_accounts: self.union.len(),
        });
        self.previous = parsed.accounts;
        self.sample_count = self.samples.len();
        self.source_proven_in_this_run = true;
        self.phase = "sample_complete";
        self.status = "native_source_observed";
        true
    }

    pub fn failure(&mut self, failure: Failure) {
        self.phase = if failure.is_denial() {
            "denied"
        } else {
            "sample_failed"
        };
        self.status = failure.code();
        if self.source_errors.len() < 8 {
            self.source_errors.push(failure.code());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request() -> Selection {
        select_request(br#"[{"operationName":"Ignored","variables":{}},{"operationName":"CommunityTab","variables":{"login":"Kamet0"}}]"#, "kamet0").unwrap().unwrap()
    }
    fn response(logins: &[&str]) -> Vec<u8> {
        serde_json::to_vec(
            &json!([{"data":{}}, {"data":{"user":{"channel":{"chatters":{
                "count":67524, "broadcasters":[{"login":"Kamet0"}], "staff":[],
                "moderators":[{"login":"Bot_One"}], "vips":[], "chatbots":[{"login":"bot_one"}],
                "viewers":logins.iter().map(|login| json!({"login":login})).collect::<Vec<_>>()
            }}}}}]),
        )
        .unwrap()
    }

    #[test]
    fn selects_case_insensitive_channel_and_exact_operation() {
        let selected = request();
        assert_eq!(selected.index, 1);
        assert_eq!(selected.operation, "CommunityTab");
        assert!(select_request(
            br#"{"operationName":"CommunityTab","variables":{"login":"other"}}"#,
            "kamet0"
        )
        .unwrap()
        .is_none());
        assert!(select_request(
            br#"{"operationName":"CommunityTabExtra","variables":{"login":"kamet0"}}"#,
            "kamet0"
        )
        .unwrap()
        .is_none());
        assert!(select_request(br#"[{"operationName":"CommunityTab","variables":{"login":"kamet0"}},{"operationName":"ChatViewers","variables":{"channelLogin":"KAMET0"}}]"#, "kamet0").is_err());
    }

    #[test]
    fn preserves_categories_and_deduplicates_role_overlap() {
        let parsed = parse_response(
            &response(&["Viewer_One", "VIEWER_ONE", "viewer_two"]),
            &request(),
            200,
        )
        .unwrap();
        assert_eq!(parsed.count, 67524);
        assert_eq!(parsed.categories["viewers"], 3);
        assert_eq!(parsed.accounts.len(), 4);
        assert!(parsed.accounts.contains("viewer_one"));
    }

    #[test]
    fn aggregate_json_contains_no_accounts_or_raw_errors() {
        let selected = request();
        let mut evidence = Evidence::default();
        evidence.add(
            parse_response(&response(&["secret_viewer_one"]), &selected, 200).unwrap(),
            &selected,
            1,
        );
        evidence.add(
            parse_response(&response(&["secret_viewer_two"]), &selected, 200).unwrap(),
            &selected,
            31,
        );
        assert_eq!(evidence.samples[1].overlap_with_previous, Some(2));
        assert_eq!(evidence.samples[1].union_accounts, 4);
        let output = serde_json::to_string(&evidence).unwrap();
        assert!(!output.contains("secret_viewer"));
        assert!(!output.contains("bot_one"));
        assert!(!output.contains("Kamet0"));
        assert_eq!(evidence.samples[1].source_error_count, 0);
    }

    #[test]
    fn classifies_denials_without_returning_the_error_text() {
        for (message, expected) in [
            ("IntegrityFailed with secret", Failure::IntegrityDenied),
            ("unauthenticated secret", Failure::AuthenticationDenied),
            ("captcha secret", Failure::ChallengeDenied),
        ] {
            let body = serde_json::to_vec(&json!([{}, {"errors":[{"message":message}]}])).unwrap();
            assert_eq!(
                parse_response(&body, &request(), 200).unwrap_err(),
                expected
            );
            assert!(expected.is_denial());
        }
        assert_eq!(
            parse_response(b"not JSON", &request(), 403).unwrap_err(),
            Failure::HttpDenied
        );
    }

    #[test]
    fn rejects_oversized_malformed_and_mismatched_data() {
        assert_eq!(
            select_request(&vec![b' '; REQUEST_CAP + 1], "kamet0").unwrap_err(),
            Failure::RequestTooLarge
        );
        assert_eq!(
            parse_response(&vec![b' '; RESPONSE_CAP + 1], &request(), 200).unwrap_err(),
            Failure::ResponseTooLarge
        );
        assert_eq!(
            parse_response(b"[{}]", &request(), 200).unwrap_err(),
            Failure::BatchMismatch
        );
        assert_eq!(
            parse_response(b"[{}, {\"data\":{}}]", &request(), 200).unwrap_err(),
            Failure::MissingChatters
        );
        let mut malformed: Value = serde_json::from_slice(&response(&["invalid login"])).unwrap();
        assert_eq!(
            parse_response(&serde_json::to_vec(&malformed).unwrap(), &request(), 200).unwrap_err(),
            Failure::InvalidShape
        );
        malformed[1]["data"]["user"]["channel"]["chatters"]["viewers"] = Value::Null;
        assert_eq!(
            parse_response(&serde_json::to_vec(&malformed).unwrap(), &request(), 200).unwrap_err(),
            Failure::InvalidShape
        );
    }

    #[test]
    fn legacy_unbatched_shape_and_three_sample_bound() {
        let selected = select_request(
            br#"{"operationName":"ChatViewers","variables":{"channelLogin":"KaMeT0"}}"#,
            "kamet0",
        )
        .unwrap()
        .unwrap();
        let original: Value = serde_json::from_slice(&response(&["viewer"])).unwrap();
        let bytes = serde_json::to_vec(&json!({"data":{"channel":{"chatters":original[1]["data"]["user"]["channel"]["chatters"]}}})).unwrap();
        let mut evidence = Evidence::default();
        for timestamp in [1, 31, 61] {
            assert!(evidence.add(
                parse_response(&bytes, &selected, 200).unwrap(),
                &selected,
                timestamp
            ));
        }
        assert!(!evidence.add(
            parse_response(&bytes, &selected, 200).unwrap(),
            &selected,
            91
        ));
        assert_eq!(evidence.sample_count, 3);
    }

    #[test]
    fn strict_source_and_page_boundaries() {
        assert!(channel_login(" KaMeT0 ").is_some());
        for invalid in [
            "",
            "évil",
            "../login",
            "name?token=secret",
            "a b",
            "aaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(channel_login(invalid).is_none());
        }
        assert!(is_source_uri("https://gql.twitch.tv/gql"));
        assert!(!is_source_uri("https://gql.twitch.tv.evil/gql"));
        assert!(is_channel_page("https://www.twitch.tv/KaMeT0", "kamet0"));
        assert!(!is_channel_page("https://www.twitch.tv/login", "login"));
        assert!(!is_channel_page("https://www.twitch.tv/other", "kamet0"));
    }
}
