//! Bounded anonymous IRC membership observations. Accounts are never video-viewer evidence.
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::{timeout, timeout_at, Instant};
use tokio_tungstenite::tungstenite::{protocol::WebSocketConfig, Message};

const MAX_LINE: usize = 64 * 1024;
const MAX_BUFFER: usize = 128 * 1024;
const MAX_ACCOUNTS: usize = 10_000;
pub const EVIDENCE_SCOPE: &str = "observed chat-connected accounts; completeness unknown; large-room membership may be unavailable; not video viewers or watch time";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IrcError {
    #[error("invalid channel configuration")]
    Channels,
    #[error("IRC protocol or identity validation failed")]
    Protocol,
    #[error("IRC message or account bound exceeded")]
    Limit,
    #[error("IRC server rejected the anonymous session or channel")]
    Rejected,
    #[error("IRC server returned CAP NAK")]
    CapabilityNak,
    #[error("IRC server sent an unexpected capability response")]
    CapabilityResponse,
    #[error("IRC connection failed")]
    Connect,
    #[error("IRC transport failed or closed before observation completed")]
    Transport,
    #[error("IRC registration, initial NAMES, or overall deadline expired")]
    Timeout,
}

fn login(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= 25
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_'))
    .then(|| value.to_ascii_lowercase())
}

pub fn normalize_channels(channels: &[String]) -> Result<Vec<String>, IrcError> {
    let normalized: Result<Vec<_>, _> = channels
        .iter()
        .map(|channel| login(channel.trim()).ok_or(IrcError::Channels))
        .collect();
    let mut normalized = normalized?;
    normalized.sort();
    normalized.dedup();
    if normalized.is_empty() || normalized.len() > 3 {
        return Err(IrcError::Channels);
    }
    Ok(normalized)
}

/// IRC text can contain multiple lines or split a line across WebSocket messages.
#[derive(Default)]
pub struct LineBuffer(Vec<u8>);

impl LineBuffer {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, IrcError> {
        if self.0.len().saturating_add(bytes.len()) > MAX_BUFFER {
            return Err(IrcError::Limit);
        }
        self.0.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(end) = self.0.iter().position(|byte| *byte == b'\n') {
            if end == 0 || self.0[end - 1] != b'\r' || end - 1 > MAX_LINE {
                return Err(IrcError::Protocol);
            }
            let line = std::str::from_utf8(&self.0[..end - 1])
                .map_err(|_| IrcError::Protocol)?
                .to_owned();
            self.0.drain(..=end);
            if line.is_empty() {
                return Err(IrcError::Protocol);
            }
            lines.push(line);
        }
        if self.0.len() > MAX_LINE + 1 {
            return Err(IrcError::Limit);
        }
        Ok(lines)
    }
}

#[derive(Debug)]
struct IrcMessage<'a> {
    prefix: Option<&'a str>,
    command: &'a str,
    params: Vec<&'a str>,
}

fn parse(line: &str) -> Result<IrcMessage<'_>, IrcError> {
    if line.len() > MAX_LINE || line.chars().any(|c| c == '\0' || c == '\r' || c == '\n') {
        return Err(IrcError::Protocol);
    }
    let mut rest = line;
    if rest.starts_with('@') {
        let (_, remaining) = rest.split_once(' ').ok_or(IrcError::Protocol)?;
        if remaining.is_empty() {
            return Err(IrcError::Protocol);
        }
        rest = remaining.trim_start_matches(' ');
    }
    let prefix = if let Some(without_colon) = rest.strip_prefix(':') {
        let (prefix, remaining) = without_colon.split_once(' ').ok_or(IrcError::Protocol)?;
        if prefix.is_empty() || !prefix.is_ascii() || prefix.chars().any(char::is_control) {
            return Err(IrcError::Protocol);
        }
        rest = remaining.trim_start_matches(' ');
        Some(prefix)
    } else {
        None
    };
    let (command, remaining) = rest.split_once(' ').unwrap_or((rest, ""));
    if command.is_empty() || !command.bytes().all(|c| c.is_ascii_alphanumeric()) {
        return Err(IrcError::Protocol);
    }
    let mut params = Vec::new();
    rest = remaining.trim_start_matches(' ');
    while !rest.is_empty() {
        if params.len() >= 15 {
            return Err(IrcError::Protocol);
        }
        if let Some(trailing) = rest.strip_prefix(':') {
            params.push(trailing);
            break;
        }
        let (param, remaining) = rest.split_once(' ').unwrap_or((rest, ""));
        params.push(param);
        rest = remaining.trim_start_matches(' ');
    }
    Ok(IrcMessage {
        prefix,
        command,
        params,
    })
}

fn server(prefix: Option<&str>) -> bool {
    prefix.is_some_and(|prefix| {
        let prefix = prefix.to_ascii_lowercase();
        prefix == "tmi.twitch.tv"
            || prefix.strip_suffix(".tmi.twitch.tv").is_some_and(|name| {
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'.')
            })
    })
}

fn user(prefix: Option<&str>) -> Result<String, IrcError> {
    let prefix = prefix.ok_or(IrcError::Protocol)?;
    let (nick, remainder) = prefix.split_once('!').ok_or(IrcError::Protocol)?;
    let (ident, host) = remainder.split_once('@').ok_or(IrcError::Protocol)?;
    let nick = login(nick).ok_or(IrcError::Protocol)?;
    if !ident.eq_ignore_ascii_case(&nick)
        || !host.eq_ignore_ascii_case(&format!("{nick}.tmi.twitch.tv"))
    {
        return Err(IrcError::Protocol);
    }
    Ok(nick)
}

#[derive(Default)]
struct Room {
    names_complete: bool,
    accounts: HashSet<String>,
    departed: HashSet<String>,
    initial_accounts: usize,
    joins: usize,
    parts: usize,
}

fn insert_bounded(accounts: &mut HashSet<String>, account: String) -> Result<(), IrcError> {
    if !accounts.contains(&account) && accounts.len() >= MAX_ACCOUNTS {
        return Err(IrcError::Limit);
    }
    accounts.insert(account);
    Ok(())
}

pub struct Membership {
    guest: String,
    rooms: BTreeMap<String, Room>,
    welcome: bool,
    capability: bool,
    joined: bool,
    failure: Option<IrcError>,
}

#[derive(Debug, Serialize)]
pub struct Observation {
    pub channel: String,
    /// Observed set size at the first NAMES terminator, even if NAMES is delayed.
    pub initial_observed_accounts: usize,
    pub observed_accounts_at_end: usize,
    /// Received messages across bootstrap and the window, including possible duplicates.
    pub nonself_join_events: usize,
    /// Received messages across bootstrap and the window, including possible duplicates.
    pub nonself_part_events: usize,
    pub names_terminator_received: bool,
    pub membership_window_seconds: u64,
    pub evidence_scope: &'static str,
}

impl Membership {
    pub fn new(guest: &str, channels: &[String]) -> Result<Self, IrcError> {
        let guest = login(guest)
            .filter(|guest| {
                guest.strip_prefix("justinfan").is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
                })
            })
            .ok_or(IrcError::Protocol)?;
        Ok(Self {
            guest,
            rooms: normalize_channels(channels)?
                .into_iter()
                .map(|channel| (channel, Room::default()))
                .collect(),
            welcome: false,
            capability: false,
            joined: false,
            failure: None,
        })
    }

    pub fn ready_to_join(&self) -> bool {
        self.welcome && self.capability && !self.joined && self.failure.is_none()
    }

    pub fn start_join(&mut self) -> Result<String, IrcError> {
        if !self.ready_to_join() {
            return Err(IrcError::Protocol);
        }
        self.joined = true;
        Ok(format!(
            "JOIN {}\r\n",
            self.rooms
                .keys()
                .map(|room| format!("#{room}"))
                .collect::<Vec<_>>()
                .join(",")
        ))
    }

    pub fn all_names_complete(&self) -> bool {
        // "Complete" refers only to receipt of each protocol terminator, not coverage.
        self.rooms.values().all(|room| room.names_complete)
    }

    fn room(&mut self, raw: &str) -> Result<&mut Room, IrcError> {
        if !self.joined {
            return Err(IrcError::Protocol);
        }
        let channel =
            login(raw.strip_prefix('#').ok_or(IrcError::Protocol)?).ok_or(IrcError::Protocol)?;
        self.rooms.get_mut(&channel).ok_or(IrcError::Protocol)
    }

    /// Returns only a protocol PONG response, never incoming content for logging.
    pub fn line(&mut self, line: &str) -> Result<Option<String>, IrcError> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let result = self.apply_line(line);
        if let Err(error) = &result {
            self.failure = Some(error.clone());
        }
        result
    }

    fn apply_line(&mut self, line: &str) -> Result<Option<String>, IrcError> {
        let message = parse(line)?;
        let params = &message.params;
        match message.command.to_ascii_uppercase().as_str() {
            "PING" => {
                if params.len() != 1
                    || params[0].len() > 256
                    || params[0].chars().any(char::is_control)
                {
                    return Err(IrcError::Protocol);
                }
                return Ok(Some(format!("PONG :{}\r\n", params[0])));
            }
            "001" => {
                if !server(message.prefix)
                    || params.len() < 2
                    || !params[0].eq_ignore_ascii_case(&self.guest)
                {
                    return Err(IrcError::Protocol);
                }
                self.welcome = true;
            }
            "CAP" => {
                if !server(message.prefix)
                    || params.len() != 3
                    || !(params[0] == "*" || params[0].eq_ignore_ascii_case(&self.guest))
                {
                    return Err(IrcError::Protocol);
                }
                if params[1].eq_ignore_ascii_case("NAK") {
                    return Err(IrcError::CapabilityNak);
                }
                if !params[1].eq_ignore_ascii_case("ACK")
                    || !params[2]
                        .split_whitespace()
                        .any(|cap| cap == "twitch.tv/membership")
                {
                    return Err(IrcError::CapabilityResponse);
                }
                self.capability = true;
            }
            "353" => {
                if !server(message.prefix)
                    || params.len() != 4
                    || !params[0].eq_ignore_ascii_case(&self.guest)
                    || !["=", "*", "@"].contains(&params[1])
                {
                    return Err(IrcError::Protocol);
                }
                let guest = self.guest.clone();
                let room = self.room(params[2])?;
                for raw in params[3].split_whitespace() {
                    let account =
                        login(raw.trim_start_matches(['@', '+'])).ok_or(IrcError::Protocol)?;
                    if account != guest && !room.departed.contains(&account) {
                        insert_bounded(&mut room.accounts, account)?;
                    }
                }
            }
            "366" => {
                if !server(message.prefix)
                    || params.len() != 3
                    || !params[0].eq_ignore_ascii_case(&self.guest)
                {
                    return Err(IrcError::Protocol);
                }
                let room = self.room(params[1])?;
                if !room.names_complete {
                    room.names_complete = true;
                    room.initial_accounts = room.accounts.len();
                }
            }
            "JOIN" | "PART" => {
                let joining = message.command.eq_ignore_ascii_case("JOIN");
                if params.is_empty() || params.len() > if joining { 1 } else { 2 } {
                    return Err(IrcError::Protocol);
                }
                let account = user(message.prefix)?;
                if account == self.guest {
                    self.room(params[0])?;
                    if !joining {
                        return Err(IrcError::Rejected);
                    }
                    return Ok(None);
                }
                let room = self.room(params[0])?;
                if joining {
                    room.joins = room.joins.checked_add(1).ok_or(IrcError::Limit)?;
                    room.departed.remove(&account);
                    insert_bounded(&mut room.accounts, account)?;
                } else {
                    room.parts = room.parts.checked_add(1).ok_or(IrcError::Limit)?;
                    room.accounts.remove(&account);
                    // Retain tombstones because NAMES may arrive after its terminator.
                    insert_bounded(&mut room.departed, account)?;
                }
            }
            "NOTICE" | "ERROR" | "RECONNECT" => return Err(IrcError::Rejected),
            command if command.len() == 3 && command.bytes().all(|byte| byte.is_ascii_digit()) => {
                if !server(message.prefix) {
                    return Err(IrcError::Protocol);
                }
                if command.as_bytes()[0] >= b'4' {
                    return Err(IrcError::Rejected);
                }
            }
            _ => {}
        }
        Ok(None)
    }

    pub fn observations(&self, window: u64) -> Result<Vec<Observation>, IrcError> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if !self.all_names_complete() {
            return Err(IrcError::Protocol);
        }
        Ok(self
            .rooms
            .iter()
            .map(|(channel, room)| Observation {
                channel: channel.clone(),
                initial_observed_accounts: room.initial_accounts,
                observed_accounts_at_end: room.accounts.len(),
                nonself_join_events: room.joins,
                nonself_part_events: room.parts,
                names_terminator_received: room.names_complete,
                membership_window_seconds: window,
                evidence_scope: EVIDENCE_SCOPE,
            })
            .collect())
    }
}

/// No account credentials or chat messages are sent. The password is the public IRC guest convention.
pub async fn observe(
    channels: &[String],
    window: Duration,
    budget: Duration,
) -> Result<Vec<Observation>, IrcError> {
    if window > Duration::from_secs(300)
        || budget > Duration::from_secs(600)
        || budget < Duration::from_secs(15)
    {
        return Err(IrcError::Channels);
    }
    let guest = format!(
        "justinfan{}",
        (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| IrcError::Protocol)?
            .as_nanos()
            ^ u128::from(std::process::id()))
            % 1_000_000_000_000
    );
    observe_endpoint(
        channels,
        window,
        budget,
        &guest,
        "wss://irc-ws.chat.twitch.tv:443",
    )
    .await
}

async fn observe_endpoint(
    channels: &[String],
    window: Duration,
    budget: Duration,
    guest: &str,
    endpoint: &str,
) -> Result<Vec<Observation>, IrcError> {
    let mut membership = Membership::new(guest, channels)?;
    let overall_deadline = Instant::now() + budget;
    // Reserve time for PART and Close even when the collection deadline expires.
    let collection_deadline = overall_deadline - Duration::from_secs(4).min(budget / 4);
    let config = WebSocketConfig::default()
        .write_buffer_size(1024)
        .max_write_buffer_size(16 * 1024)
        .max_message_size(Some(MAX_BUFFER))
        .max_frame_size(Some(MAX_BUFFER));
    let (mut socket, _) = timeout_at(
        collection_deadline.min(Instant::now() + Duration::from_secs(10)),
        tokio_tungstenite::connect_async_with_config(endpoint, Some(config), false),
    )
    .await
    .map_err(|_| IrcError::Timeout)?
    .map_err(|_| IrcError::Connect)?;
    let result = timeout_at(collection_deadline, async {
        // Twitch accepts one client command per WebSocket message, even though
        // its incoming messages may contain several CRLF-delimited commands.
        for command in [
            "CAP REQ :twitch.tv/membership\r\n".to_owned(),
            "PASS SCHMOOPIIE\r\n".to_owned(),
            format!("NICK {guest}\r\n"),
        ] {
            timeout(Duration::from_secs(5), socket.send(Message::text(command)))
                .await
                .map_err(|_| IrcError::Timeout)?
                .map_err(|_| IrcError::Transport)?;
        }
        let mut deadline = Instant::now() + Duration::from_secs(10);
        let mut observing = false;
        let mut lines = LineBuffer::default();
        loop {
            // A ready socket can otherwise win a timeout repeatedly under heavy traffic.
            if Instant::now() >= collection_deadline {
                return Err(IrcError::Timeout);
            }
            if Instant::now() >= deadline {
                if observing {
                    break;
                }
                return Err(IrcError::Timeout);
            }
            let received = match timeout_at(deadline, socket.next()).await {
                Ok(Some(Ok(message))) => message,
                Ok(_) => return Err(IrcError::Transport),
                Err(_) if observing => break,
                Err(_) => return Err(IrcError::Timeout),
            };
            match received {
                Message::Text(text) => {
                    for line in lines.push(text.as_bytes())? {
                        if Instant::now() >= collection_deadline {
                            return Err(IrcError::Timeout);
                        }
                        if let Some(pong) = membership.line(&line)? {
                            timeout(Duration::from_secs(5), socket.send(Message::text(pong)))
                                .await
                                .map_err(|_| IrcError::Timeout)?
                                .map_err(|_| IrcError::Transport)?;
                        }
                        if membership.ready_to_join() {
                            let join = membership.start_join()?;
                            timeout(Duration::from_secs(5), socket.send(Message::text(join)))
                                .await
                                .map_err(|_| IrcError::Timeout)?
                                .map_err(|_| IrcError::Transport)?;
                            deadline = Instant::now() + Duration::from_secs(15);
                        }
                    }
                }
                Message::Ping(_) => {
                    // Tungstenite queues the matching WebSocket Pong automatically.
                    timeout(Duration::from_secs(5), socket.flush())
                        .await
                        .map_err(|_| IrcError::Timeout)?
                        .map_err(|_| IrcError::Transport)?;
                }
                Message::Pong(_) => {}
                _ => return Err(IrcError::Transport),
            }
            if membership.all_names_complete() && !observing {
                observing = true;
                deadline = Instant::now() + window;
            }
        }
        if !lines.is_empty() {
            return Err(IrcError::Protocol);
        }
        membership.observations(window.as_secs())
    })
    .await
    .unwrap_or(Err(IrcError::Timeout));
    if membership.joined {
        let part = format!(
            "PART {}\r\n",
            membership
                .rooms
                .keys()
                .map(|room| format!("#{room}"))
                .collect::<Vec<_>>()
                .join(",")
        );
        let _ = timeout_at(
            overall_deadline.min(Instant::now() + Duration::from_secs(2)),
            socket.send(Message::text(part)),
        )
        .await;
    }
    let _ = timeout_at(
        overall_deadline.min(Instant::now() + Duration::from_secs(2)),
        socket.close(None),
    )
    .await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn expect_registration(
        server: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    ) {
        for expected in [
            "CAP REQ :twitch.tv/membership\r\n",
            "PASS SCHMOOPIIE\r\n",
            "NICK justinfan123\r\n",
        ] {
            let actual = timeout(Duration::from_secs(2), server.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(actual, Message::text(expected));
        }
    }

    fn joined() -> Membership {
        let mut state = Membership::new("justinfan123", &["test".into()]).unwrap();
        state
            .line(":tmi.twitch.tv CAP * ACK :twitch.tv/membership")
            .unwrap();
        state
            .line(":tmi.twitch.tv 001 justinfan123 :Welcome")
            .unwrap();
        assert_eq!(state.start_join().unwrap(), "JOIN #test\r\n");
        state
    }
    #[test]
    fn fragmented_tagged_unicode_lines_and_bounds() {
        let mut buffer = LineBuffer::default();
        assert!(buffer.push(b"@display-name=\xc3").unwrap().is_empty());
        assert!(buffer
            .push(b"\xa9 :tmi.twitch.tv 001 justinfan123 :Welcome\r")
            .unwrap()
            .is_empty());
        assert_eq!(buffer.push(b"\nPING :server\r\n").unwrap().len(), 2);
        assert!(LineBuffer::default().push(b"PING :bad\n").is_err());
        assert!(LineBuffer::default()
            .push(&vec![b'x'; MAX_BUFFER + 1])
            .is_err());
        assert!(parse(":bad\0prefix PING :x").is_err());
        assert!(LineBuffer::default().push(b"PING :\xff\r\n").is_err());
        assert!(LineBuffer::default()
            .push(&vec![b'x'; MAX_LINE + 2])
            .is_err());
    }
    #[test]
    fn chunks_case_duplicates_roles_and_self_are_handled() {
        let mut state = joined();
        state.line("@tag=anything :bar.tmi.twitch.tv 353 JUSTINFAN123 = #TEST :@Alice +alice BOB justinfan123").unwrap();
        state
            .line(":tmi.twitch.tv 353 justinfan123 = #test :bob Carol")
            .unwrap();
        state
            .line(":tmi.twitch.tv 366 justinfan123 #test :End")
            .unwrap();
        let report = state.observations(30).unwrap();
        assert_eq!(report[0].initial_observed_accounts, 3);
        assert!(!serde_json::to_string(&report).unwrap().contains("alice"));
    }
    #[test]
    fn orphan_wrong_channel_identity_and_malformed_prefix_fail() {
        for line in [
            ":tmi.twitch.tv 366 other #test :End",
            ":tmi.twitch.tv 366 justinfan123 #wrong :End",
            ":tmi.twitch.tv 353 other = #test :alice",
            ":tmi.twitch.tv 353 justinfan123 = #wrong :alice",
            ":fake.example 353 justinfan123 = #test :alice",
            ":alice!other@alice.tmi.twitch.tv JOIN #test",
            ":alice JOIN #test",
            ":tmi.twitch.tv 353 justinfan123 = #test :not-valid",
        ] {
            assert!(joined().line(line).is_err(), "unexpected accepted fixture");
        }
        let mut state = Membership::new("justinfan123", &["test".into()]).unwrap();
        assert!(state
            .line(":tmi.twitch.tv 353 justinfan123 = #test :alice")
            .is_err());
    }

    #[test]
    fn reordered_names_and_duplicate_messages_preserve_observed_departures() {
        let mut state = joined();
        state
            .line(":tmi.twitch.tv 001 justinfan123 :Welcome again")
            .unwrap();
        assert!(!state.ready_to_join());
        state
            .line(":tmi.twitch.tv 366 justinfan123 #test :End before NAMES")
            .unwrap();
        state
            .line(":tmi.twitch.tv 353 justinfan123 = #test :alice bob")
            .unwrap();
        state
            .line(":alice!alice@alice.tmi.twitch.tv PART #test")
            .unwrap();
        state
            .line(":tmi.twitch.tv 353 justinfan123 = #test :alice bob")
            .unwrap();
        state
            .line(":tmi.twitch.tv 366 justinfan123 #test :Duplicate end")
            .unwrap();
        let report = state.observations(30).unwrap();
        assert_eq!(report[0].initial_observed_accounts, 0);
        assert_eq!(report[0].observed_accounts_at_end, 1);
        state
            .line(":alice!alice@alice.tmi.twitch.tv JOIN #test")
            .unwrap();
        state
            .line(":alice!alice@alice.tmi.twitch.tv JOIN #test")
            .unwrap();
        state.line(":bob!bob@bob.tmi.twitch.tv PART #test").unwrap();
        state.line(":bob!bob@bob.tmi.twitch.tv PART #test").unwrap();
        state
            .line(":tmi.twitch.tv 353 justinfan123 = #test :alice bob")
            .unwrap();
        let report = state.observations(30).unwrap();
        assert_eq!(report[0].observed_accounts_at_end, 1);
        assert_eq!(report[0].nonself_join_events, 2);
        assert_eq!(report[0].nonself_part_events, 3);
    }

    #[test]
    fn account_and_tombstone_limits_stop_without_partial_observations() {
        for joining in [true, false] {
            let mut state = joined();
            state
                .line(":tmi.twitch.tv 366 justinfan123 #test :End")
                .unwrap();
            let command = if joining { "JOIN" } else { "PART" };
            for index in 0..MAX_ACCOUNTS {
                state.line(&format!(":account{index}!account{index}@account{index}.tmi.twitch.tv {command} #test")).unwrap();
            }
            let duplicate = format!(":account0!account0@account0.tmi.twitch.tv {command} #test");
            state.line(&duplicate).unwrap();
            assert_eq!(
                state.line(&format!(":extra!extra@extra.tmi.twitch.tv {command} #test")),
                Err(IrcError::Limit)
            );
            assert_eq!(
                state.rooms["test"].accounts.len(),
                if joining { MAX_ACCOUNTS } else { 0 }
            );
            assert_eq!(
                state.rooms["test"].departed.len(),
                if joining { 0 } else { MAX_ACCOUNTS }
            );
            assert!(matches!(state.observations(30), Err(IrcError::Limit)));
        }
    }

    #[test]
    fn self_part_invalidates_observations_but_other_guests_are_counted() {
        let mut state = joined();
        state
            .line(":tmi.twitch.tv 353 justinfan123 = #test :justinfan123 justinfan999")
            .unwrap();
        state
            .line(":tmi.twitch.tv 366 justinfan123 #test :End")
            .unwrap();
        assert_eq!(
            state.observations(0).unwrap()[0].observed_accounts_at_end,
            1
        );
        assert_eq!(
            state.line(":justinfan123!justinfan123@justinfan123.tmi.twitch.tv PART #test"),
            Err(IrcError::Rejected)
        );
        assert!(matches!(state.observations(0), Err(IrcError::Rejected)));
    }
    #[test]
    fn self_only_names_can_be_followed_by_membership_observations() {
        let mut state = joined();
        state
            .line(":alice!alice@alice.tmi.twitch.tv JOIN #test")
            .unwrap();
        state.line(":bob!bob@bob.tmi.twitch.tv PART #test").unwrap();
        state
            .line(":tmi.twitch.tv 353 justinfan123 = #test :justinfan123 bob")
            .unwrap();
        state
            .line(":tmi.twitch.tv 366 justinfan123 #test :End")
            .unwrap();
        state
            .line(":carol!carol@carol.tmi.twitch.tv JOIN #test")
            .unwrap();
        state
            .line(":alice!alice@alice.tmi.twitch.tv PART #test :bye")
            .unwrap();
        let report = state.observations(30).unwrap();
        assert_eq!(report[0].initial_observed_accounts, 1);
        assert_eq!(report[0].observed_accounts_at_end, 1);
        assert_eq!(report[0].nonself_join_events, 2);
        assert_eq!(report[0].nonself_part_events, 2);
    }
    #[test]
    fn capability_errors_ping_and_rejections() {
        assert_eq!(
            joined().line("PING :tmi.twitch.tv").unwrap(),
            Some("PONG :tmi.twitch.tv\r\n".into())
        );
        assert_eq!(
            joined().line(":tmi.twitch.tv CAP * NAK :twitch.tv/membership"),
            Err(IrcError::CapabilityNak)
        );
        assert_eq!(
            joined().line(":tmi.twitch.tv CAP * ACK :twitch.tv/commands"),
            Err(IrcError::CapabilityResponse)
        );
        assert_eq!(
            joined().line(":tmi.twitch.tv CAP * LS :twitch.tv/membership"),
            Err(IrcError::CapabilityResponse)
        );
        assert_eq!(
            joined().line(":tmi.twitch.tv NOTICE * :Login authentication failed"),
            Err(IrcError::Rejected)
        );
        assert_eq!(
            joined().line(":tmi.twitch.tv 474 justinfan123 #test :Cannot join"),
            Err(IrcError::Rejected)
        );
        assert!(normalize_channels(&["x\r\nJOIN #bad".into()]).is_err());
        assert!(
            normalize_channels(&["one".into(), "two".into(), "three".into(), "four".into()])
                .is_err()
        );
        assert_eq!(
            normalize_channels(&[" Test ".into(), "test".into()]).unwrap(),
            vec!["test"]
        );
        assert!(Membership::new("justinfan", &["test".into()]).is_err());
        assert!(Membership::new("justinfanabc", &["test".into()]).is_err());
    }

    #[test]
    fn self_only_terminator_is_not_a_complete_or_empty_population_claim() {
        let mut state = joined();
        state
            .line(":tmi.twitch.tv 353 justinfan123 = #test :justinfan123")
            .unwrap();
        state
            .line(":tmi.twitch.tv 366 justinfan123 #test :End")
            .unwrap();
        assert_eq!(
            state.observations(0).unwrap()[0].observed_accounts_at_end,
            0
        );
        for index in 0..128 {
            state
                .line(&format!(
                    ":account{index}!account{index}@account{index}.tmi.twitch.tv JOIN #test"
                ))
                .unwrap();
        }
        let report = state.observations(30).unwrap();
        assert_eq!(report[0].initial_observed_accounts, 0);
        assert_eq!(report[0].observed_accounts_at_end, 128);
        assert_eq!(report[0].nonself_join_events, 128);
        assert!(report[0].evidence_scope.contains("completeness unknown"));
    }

    #[test]
    fn room_snapshots_and_events_remain_independent() {
        let mut state = Membership::new("justinfan123", &["test".into(), "other".into()]).unwrap();
        state
            .line(":tmi.twitch.tv CAP * ACK :twitch.tv/membership")
            .unwrap();
        state
            .line(":tmi.twitch.tv 001 justinfan123 :Welcome")
            .unwrap();
        assert_eq!(state.start_join().unwrap(), "JOIN #other,#test\r\n");
        state
            .line(":tmi.twitch.tv 353 justinfan123 = #test :alice justinfan999")
            .unwrap();
        state
            .line(":tmi.twitch.tv 366 justinfan123 #test :End")
            .unwrap();
        assert!(!state.all_names_complete());
        state
            .line(":tmi.twitch.tv 353 justinfan123 = #other :alice")
            .unwrap();
        state
            .line(":tmi.twitch.tv 366 justinfan123 #other :End")
            .unwrap();
        state
            .line(":alice!alice@alice.tmi.twitch.tv PART #other")
            .unwrap();
        let reports = state.observations(30).unwrap();
        assert_eq!(reports[0].observed_accounts_at_end, 0);
        assert_eq!(reports[1].observed_accounts_at_end, 2);
    }

    #[tokio::test]
    async fn offline_registration_accepts_separate_ack_and_welcome_in_either_order() {
        for welcome_first in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("ws://{}", listener.local_addr().unwrap());
            let server_task = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut server = tokio_tungstenite::accept_async(stream).await.unwrap();
                expect_registration(&mut server).await;
                let welcome = ":tmi.twitch.tv 001 justinfan123 :Welcome\r\n";
                let ack = ":tmi.twitch.tv CAP * ACK :twitch.tv/membership\r\n";
                let messages = if welcome_first {
                    [welcome, ack]
                } else {
                    [ack, welcome]
                };
                server.send(Message::text(messages[0])).await.unwrap();
                // A single prerequisite must not trigger JOIN.
                assert!(timeout(Duration::from_millis(50), server.next())
                    .await
                    .is_err());
                server.send(Message::text(messages[1])).await.unwrap();
                assert_eq!(
                    server.next().await.unwrap().unwrap(),
                    Message::text("JOIN #test\r\n")
                );
                server
                    .send(Message::text(
                        ":tmi.twitch.tv 366 justinfan123 #test :End\r\n",
                    ))
                    .await
                    .unwrap();
                assert_eq!(
                    server.next().await.unwrap().unwrap(),
                    Message::text("PART #test\r\n")
                );
                assert!(matches!(
                    server.next().await.unwrap().unwrap(),
                    Message::Close(_)
                ));
            });
            let result = observe_endpoint(
                &["test".into()],
                Duration::ZERO,
                Duration::from_secs(5),
                "justinfan123",
                &endpoint,
            )
            .await
            .unwrap();
            assert_eq!(result[0].observed_accounts_at_end, 0);
            timeout(Duration::from_secs(2), server_task)
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn offline_cap_nak_closes_without_joining_or_echoing_payload() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let server_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut server = tokio_tungstenite::accept_async(stream).await.unwrap();
            expect_registration(&mut server).await;
            server
                .send(Message::text(
                    ":tmi.twitch.tv CAP * NAK :untrusted_server_payload\r\n",
                ))
                .await
                .unwrap();
            assert!(matches!(
                server.next().await.unwrap().unwrap(),
                Message::Close(_)
            ));
        });
        let error = observe_endpoint(
            &["test".into()],
            Duration::ZERO,
            Duration::from_secs(5),
            "justinfan123",
            &endpoint,
        )
        .await
        .unwrap_err();
        assert_eq!(error, IrcError::CapabilityNak);
        assert_eq!(error.to_string(), "IRC server returned CAP NAK");
        timeout(Duration::from_secs(2), server_task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn offline_transport_handles_both_ping_types_and_graceful_part() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let server_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut server = tokio_tungstenite::accept_async(stream).await.unwrap();
            expect_registration(&mut server).await;
            server.send(Message::text(":tmi.twitch.tv CAP * ACK :twitch.tv/membership\r\n:tmi.twitch.tv 001 justinfan123 :Welcome\r\n")).await.unwrap();
            assert_eq!(
                server.next().await.unwrap().unwrap().into_text().unwrap(),
                "JOIN #test\r\n"
            );
            server.send(Message::text(":tmi.twitch.tv 353 justinfan123 = #test :justinfan123\r\n:tmi.twitch.tv 366 justinfan123 #test :End\r\n")).await.unwrap();
            server
                .send(Message::Ping(vec![1, 2, 3].into()))
                .await
                .unwrap();
            assert_eq!(
                server.next().await.unwrap().unwrap(),
                Message::Pong(vec![1, 2, 3].into())
            );
            server
                .send(Message::text(
                    "PING :tmi.twitch.tv\r\n:alice!alice@alice.tmi.twitch.tv JOIN #test\r\n",
                ))
                .await
                .unwrap();
            assert_eq!(
                server.next().await.unwrap().unwrap().into_text().unwrap(),
                "PONG :tmi.twitch.tv\r\n"
            );
            assert_eq!(
                server.next().await.unwrap().unwrap().into_text().unwrap(),
                "PART #test\r\n"
            );
            assert!(matches!(
                server.next().await.unwrap().unwrap(),
                Message::Close(_)
            ));
        });
        let observations = observe_endpoint(
            &["test".into()],
            Duration::from_secs(2),
            Duration::from_secs(8),
            "justinfan123",
            &endpoint,
        )
        .await
        .unwrap();
        assert_eq!(observations[0].initial_observed_accounts, 0);
        assert_eq!(observations[0].observed_accounts_at_end, 1);
        timeout(Duration::from_secs(2), server_task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn offline_disconnect_and_deadline_produce_no_successful_observation() {
        for disconnect in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("ws://{}", listener.local_addr().unwrap());
            let server_task = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut server = tokio_tungstenite::accept_async(stream).await.unwrap();
                expect_registration(&mut server).await;
                if disconnect {
                    server.close(None).await.unwrap();
                } else {
                    tokio::time::sleep(Duration::from_millis(600)).await;
                }
            });
            let result = observe_endpoint(
                &["test".into()],
                Duration::from_millis(10),
                Duration::from_millis(500),
                "justinfan123",
                &endpoint,
            )
            .await;
            assert!(matches!(
                result,
                Err(IrcError::Transport) | Err(IrcError::Timeout)
            ));
            timeout(Duration::from_secs(2), server_task)
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn offline_deadline_after_join_still_attempts_part_and_close() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let server_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut server = tokio_tungstenite::accept_async(stream).await.unwrap();
            expect_registration(&mut server).await;
            server.send(Message::text(":tmi.twitch.tv CAP * ACK :twitch.tv/membership\r\n:tmi.twitch.tv 001 justinfan123 :Welcome\r\n")).await.unwrap();
            assert_eq!(
                server.next().await.unwrap().unwrap().into_text().unwrap(),
                "JOIN #test\r\n"
            );
            assert_eq!(
                server.next().await.unwrap().unwrap().into_text().unwrap(),
                "PART #test\r\n"
            );
            assert!(matches!(
                server.next().await.unwrap().unwrap(),
                Message::Close(_)
            ));
        });
        let result = observe_endpoint(
            &["test".into()],
            Duration::from_secs(2),
            Duration::from_secs(1),
            "justinfan123",
            &endpoint,
        )
        .await;
        assert!(matches!(result, Err(IrcError::Timeout)));
        timeout(Duration::from_secs(2), server_task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn offline_incomplete_tail_and_disconnect_after_names_invalidate_result() {
        for disconnect in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("ws://{}", listener.local_addr().unwrap());
            let server_task = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut server = tokio_tungstenite::accept_async(stream).await.unwrap();
                expect_registration(&mut server).await;
                server.send(Message::text(":tmi.twitch.tv CAP * ACK :twitch.tv/membership\r\n:tmi.twitch.tv 001 justinfan123 :Welcome\r\n")).await.unwrap();
                let _ = server.next().await;
                server.send(Message::text(":tmi.twitch.tv 353 justinfan123 = #test :alice\r\n:tmi.twitch.tv 366 justinfan123 #test :End\r\n")).await.unwrap();
                if disconnect {
                    server.close(None).await.unwrap();
                } else {
                    server
                        .send(Message::text(":alice!alice@alice.tmi.twitch.tv PART #te"))
                        .await
                        .unwrap();
                    assert_eq!(
                        server.next().await.unwrap().unwrap().into_text().unwrap(),
                        "PART #test\r\n"
                    );
                    assert!(matches!(
                        server.next().await.unwrap().unwrap(),
                        Message::Close(_)
                    ));
                }
            });
            let result = observe_endpoint(
                &["test".into()],
                Duration::from_millis(500),
                Duration::from_secs(5),
                "justinfan123",
                &endpoint,
            )
            .await;
            assert!(matches!(
                result,
                Err(IrcError::Transport) | Err(IrcError::Protocol)
            ));
            timeout(Duration::from_secs(2), server_task)
                .await
                .unwrap()
                .unwrap();
        }
    }
}
