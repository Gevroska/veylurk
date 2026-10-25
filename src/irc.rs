//! Bounded anonymous IRC membership observations. Accounts are never video-viewer evidence.
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore};
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
    #[error("observation delivery stalled; some received evidence may be missing")]
    Backpressure,
    #[error("collection stopped before received evidence was delivered; some received evidence may be missing")]
    CancelledEvidence,
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
    normalize_channels_with_limit(channels, 3)
}

fn normalize_channels_with_limit(
    channels: &[String],
    maximum: usize,
) -> Result<Vec<String>, IrcError> {
    let normalized: Result<Vec<_>, _> = channels
        .iter()
        .map(|channel| login(channel.trim()).ok_or(IrcError::Channels))
        .collect();
    let mut normalized = normalized?;
    normalized.sort();
    normalized.dedup();
    if normalized.is_empty() || normalized.len() > maximum {
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
        Self::with_limit(guest, channels, 3)
    }

    fn with_limit(guest: &str, channels: &[String], maximum: usize) -> Result<Self, IrcError> {
        let guest = login(guest)
            .filter(|guest| {
                guest.strip_prefix("justinfan").is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
                })
            })
            .ok_or(IrcError::Protocol)?;
        Ok(Self {
            guest,
            rooms: normalize_channels_with_limit(channels, maximum)?
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

/// Durable evidence messages for continuous collection, without an inferred population.
#[derive(Debug)]
pub enum StreamEvent {
    Connected,
    Observations(Vec<crate::store::NewObservation>),
    RoomJoining(String),
    RoomLeft(String),
    RoomReady(String),
    RoomUnavailable(String),
}

pub(crate) const RECEIVE_BUFFER_UNITS: usize = 32_768;
const MAX_RECEIPT_CHUNK: usize = 256;

/// A receipt stays charged while queued, pending, or in a storage transaction.
/// The collector releases its permit only after the corresponding commit.
#[derive(Debug)]
pub(crate) struct QueuedEvent {
    event: StreamEvent,
    permit: OwnedSemaphorePermit,
    received_at: String,
}

impl QueuedEvent {
    pub(crate) fn received_at(&self) -> &str {
        &self.received_at
    }

    pub(crate) fn into_parts(self) -> (StreamEvent, OwnedSemaphorePermit) {
        (self.event, self.permit)
    }
}

#[derive(Clone)]
pub(crate) struct ReceiptSender {
    events: mpsc::Sender<QueuedEvent>,
    budget: Arc<Semaphore>,
    end_time: Arc<Mutex<Option<String>>>,
}

impl ReceiptSender {
    /// Metrics may retain this handle without keeping the event channel open.
    pub(crate) fn budget_handle(&self) -> Arc<Semaphore> {
        Arc::clone(&self.budget)
    }

    /// The terminal cutoff excludes admission waits and PART/Close cleanup.
    /// Retaining this clock does not keep the event channel open.
    pub(crate) fn end_time_handle(&self) -> Arc<Mutex<Option<String>>> {
        Arc::clone(&self.end_time)
    }

    fn record_end_at(&self, received_at: &str) {
        let mut cutoff = self
            .end_time
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // All transport timestamps use the same UTC RFC 3339 representation.
        if cutoff.as_ref().is_none_or(|at| received_at < at.as_str()) {
            *cutoff = Some(received_at.to_owned());
        }
    }
}

pub(crate) fn receipt_queue() -> (ReceiptSender, mpsc::Receiver<QueuedEvent>) {
    let (events, receiver) = mpsc::channel(RECEIVE_BUFFER_UNITS);
    (
        ReceiptSender {
            events,
            budget: Arc::new(Semaphore::new(RECEIVE_BUFFER_UNITS)),
            end_time: Arc::new(Mutex::new(None)),
        },
        receiver,
    )
}

enum EventSink {
    Legacy(mpsc::Sender<StreamEvent>),
    Buffered(ReceiptSender),
}

impl EventSink {
    fn record_end_at(&self, received_at: &str) {
        if let Self::Buffered(sender) = self {
            sender.record_end_at(received_at);
        }
    }

    /// `false` means clean cancellation with no received evidence discarded.
    /// Admission and sending share one deadline, including both queue bounds.
    async fn deliver(
        &self,
        event: StreamEvent,
        stop: &mut watch::Receiver<bool>,
    ) -> Result<bool, IrcError> {
        let received_at = match &event {
            StreamEvent::Observations(rows) if !rows.is_empty() => rows[0].received_at.clone(),
            _ => crate::store::now(),
        };
        self.deliver_at(event, &received_at, stop).await
    }

    async fn deliver_at(
        &self,
        event: StreamEvent,
        received_at: &str,
        stop: &mut watch::Receiver<bool>,
    ) -> Result<bool, IrcError> {
        let received_at = match &event {
            StreamEvent::Observations(rows) if !rows.is_empty() => rows[0].received_at.as_str(),
            _ => received_at,
        }
        .to_owned();
        // RoomReady precedes its already-parsed names_end row; cancelling it
        // would discard that row too. Gap controls must not disappear cleanly.
        let evidence_at_risk = match &event {
            StreamEvent::Observations(rows) => !rows.is_empty(),
            StreamEvent::RoomLeft(_)
            | StreamEvent::RoomUnavailable(_)
            | StreamEvent::RoomReady(_) => true,
            _ => false,
        };
        let delivery = async {
            match self {
                Self::Legacy(events) => events.send(event).await.map_err(|_| IrcError::Transport),
                Self::Buffered(sender) => {
                    let weight = match &event {
                        StreamEvent::Observations(rows) => {
                            if rows.len() > MAX_RECEIPT_CHUNK {
                                return Err(IrcError::Limit);
                            }
                            rows.len().max(1)
                        }
                        _ => 1,
                    };
                    let permit = Arc::clone(&sender.budget)
                        .acquire_many_owned(weight as u32)
                        .await
                        .map_err(|_| IrcError::Transport)?;
                    sender
                        .events
                        .send(QueuedEvent {
                            event,
                            permit,
                            received_at: received_at.clone(),
                        })
                        .await
                        .map_err(|_| IrcError::Transport)
                }
            }
        };
        let result = tokio::select! {
            biased;
            _ = wait_for_stop(stop) => {
                if evidence_at_risk { Err(IrcError::CancelledEvidence) } else { Ok(false) }
            },
            result = timeout_at(Instant::now() + Duration::from_secs(5), delivery) => {
                result.unwrap_or(Err(IrcError::Backpressure)).map(|()| true)
            }
        };
        if !matches!(result, Ok(true)) {
            self.record_end_at(&received_at);
        }
        result
    }
}

async fn wait_for_stop(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow_and_update() {
            return;
        }
        if stop.changed().await.is_err() {
            return;
        }
    }
}

/// One connection, at most fifty rooms, paced joins and bounded delivery backpressure.
pub async fn stream(
    channels: Vec<String>,
    events: tokio::sync::mpsc::Sender<StreamEvent>,
    stop: tokio::sync::watch::Receiver<bool>,
) -> Result<(), IrcError> {
    stream_endpoint(channels, events, stop, "wss://irc-ws.chat.twitch.tv:443").await
}

pub(crate) async fn stream_endpoint(
    channels: Vec<String>,
    events: tokio::sync::mpsc::Sender<StreamEvent>,
    stop: tokio::sync::watch::Receiver<bool>,
    endpoint: &str,
) -> Result<(), IrcError> {
    stream_configured(channels, events, stop, endpoint, Duration::from_secs(30)).await
}

async fn stream_configured(
    channels: Vec<String>,
    events: tokio::sync::mpsc::Sender<StreamEvent>,
    stop: tokio::sync::watch::Receiver<bool>,
    endpoint: &str,
    room_timeout: Duration,
) -> Result<(), IrcError> {
    stream_managed(
        channels,
        EventSink::Legacy(events),
        stop,
        endpoint,
        room_timeout,
        None,
        Duration::from_secs(60),
        Duration::from_secs(6),
    )
    .await
}

pub(crate) async fn stream_dynamic_endpoint(
    channels: tokio::sync::watch::Receiver<Vec<String>>,
    events: ReceiptSender,
    stop: tokio::sync::watch::Receiver<bool>,
    endpoint: &str,
) -> Result<(), IrcError> {
    stream_dynamic_configured(
        channels,
        events,
        stop,
        endpoint,
        Duration::from_secs(30),
        Duration::from_secs(6),
    )
    .await
}

pub(crate) async fn stream_dynamic_configured(
    mut channels: tokio::sync::watch::Receiver<Vec<String>>,
    events: ReceiptSender,
    stop: tokio::sync::watch::Receiver<bool>,
    endpoint: &str,
    room_timeout: Duration,
    join_interval: Duration,
) -> Result<(), IrcError> {
    let initial = channels.borrow_and_update().clone();
    stream_managed(
        initial,
        EventSink::Buffered(events),
        stop,
        endpoint,
        room_timeout,
        Some(channels),
        Duration::from_secs(60),
        join_interval,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn stream_managed(
    channels: Vec<String>,
    events: EventSink,
    mut stop: tokio::sync::watch::Receiver<bool>,
    endpoint: &str,
    room_timeout: Duration,
    mut changes: Option<tokio::sync::watch::Receiver<Vec<String>>>,
    retry_base: Duration,
    join_interval: Duration,
) -> Result<(), IrcError> {
    let channels = normalize_channels_with_limit(&channels, 50)?;
    let mut desired = channels.clone();
    let guest = format!(
        "justinfan{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| IrcError::Protocol)?
            .as_nanos()
            % 1_000_000_000_000
    );
    let mut state = Membership::with_limit(&guest, &channels, 50)?;
    let config = WebSocketConfig::default()
        .write_buffer_size(1024)
        .max_write_buffer_size(16 * 1024)
        .max_message_size(Some(MAX_BUFFER))
        .max_frame_size(Some(MAX_BUFFER));
    if *stop.borrow() {
        events.record_end_at(&crate::store::now());
        return Ok(());
    }
    let connected = tokio::select! {
        _=wait_for_stop(&mut stop)=>{
            events.record_end_at(&crate::store::now());
            return Ok(());
        },
        result=timeout(Duration::from_secs(10),tokio_tungstenite::connect_async_with_config(endpoint,Some(config),false))=>result,
    };
    let (mut socket, _) = match connected {
        Ok(Ok(connection)) => connection,
        result => {
            events.record_end_at(&crate::store::now());
            return Err(if result.is_err() {
                IrcError::Timeout
            } else {
                IrcError::Connect
            });
        }
    };
    let mut joined = Vec::<String>::new();
    let result=async {
        macro_rules! deliver {
            ($event:expr) => {
                if !events.deliver($event, &mut stop).await? {
                    return Ok(());
                }
            };
        }
        macro_rules! deliver_at {
            ($event:expr, $received_at:expr) => {
                if !events.deliver_at($event, $received_at, &mut stop).await? {
                    return Ok(());
                }
            };
        }
        for command in ["CAP REQ :twitch.tv/membership\r\n".to_owned(),"PASS SCHMOOPIIE\r\n".to_owned(),format!("NICK {guest}\r\n")] {
            timeout(Duration::from_secs(5),socket.send(Message::text(command))).await.map_err(|_|IrcError::Timeout)?.map_err(|_|IrcError::Transport)?;
        }
        let registration_deadline=Instant::now()+Duration::from_secs(15);
        let mut next_join=Instant::now();
        let mut registered=false;
        let mut names_deadlines=BTreeMap::<String,Instant>::new();
        let mut ready=HashSet::<String>::new();
        let mut retry_at=BTreeMap::<String,Instant>::new();
        let mut attempts=BTreeMap::<String,u32>::new();
        let mut buffer=LineBuffer::default();
        let mut last_received=Instant::now();
        let mut heartbeat=tokio::time::interval(Duration::from_millis(100));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if *stop.borrow() { return Ok(()); }
            tokio::select! {
                _=stop.changed()=>return Ok(()),
                update=async {match &mut changes {Some(receiver)=>receiver.changed().await,None=>std::future::pending().await}}=> {
                    if update.is_err() {return Ok(());}
                    let next=changes.as_mut().expect("configuration receiver").borrow_and_update().clone();
                    desired=if next.is_empty() {Vec::new()} else {normalize_channels_with_limit(&next,50)?};
                    let removed=joined.iter().filter(|channel|!desired.contains(channel)).cloned().collect::<Vec<_>>();
                    let decision_at=crate::store::now();
                    if !removed.is_empty() {
                        let command=format!("PART {}\r\n",removed.iter().map(|channel|format!("#{channel}")).collect::<Vec<_>>().join(","));
                        timeout(Duration::from_secs(5),socket.send(Message::text(command))).await.map_err(|_|IrcError::Timeout)?.map_err(|_|IrcError::Transport)?;
                    }
                    for channel in removed {
                        joined.retain(|current|current!=&channel);
                        names_deadlines.remove(&channel);ready.remove(&channel);state.rooms.remove(&channel);
                        deliver_at!(StreamEvent::RoomLeft(channel), &decision_at);
                    }
                    retry_at.retain(|channel,_|desired.contains(channel));
                    attempts.retain(|channel,_|desired.contains(channel));
                    state.rooms.retain(|channel,_|desired.contains(channel));
                    for channel in &desired {state.rooms.entry(channel.clone()).or_default();}
                }
                _=heartbeat.tick()=> {
                    let now=Instant::now();
                    let decision_at=crate::store::now();
                    if !registered && now>=registration_deadline { return Err(IrcError::Timeout); }
                    if now.duration_since(last_received)>Duration::from_secs(360) { return Err(IrcError::Timeout); }
                    let unavailable=names_deadlines.iter().filter(|(_,deadline)|now>=**deadline).map(|(channel,_)|channel.clone()).collect::<Vec<_>>();
                    for channel in unavailable {
                        names_deadlines.remove(&channel);
                        ready.remove(&channel);
                        deliver_at!(StreamEvent::RoomUnavailable(channel.clone()), &decision_at);
                        if changes.is_some() {
                            let command=format!("PART #{channel}\r\n");
                            timeout(Duration::from_secs(5),socket.send(Message::text(command))).await.map_err(|_|IrcError::Timeout)?.map_err(|_|IrcError::Transport)?;
                            joined.retain(|current|current!=&channel);
                            let count=attempts.entry(channel.clone()).or_default();*count=(*count+1).min(4);
                            retry_at.insert(channel.clone(),now+retry_base.saturating_mul(1<<(*count-1)));
                            state.rooms.insert(channel,Room::default());
                        }
                    }
                    // Ten channels per six seconds remains below the documented 20/10s limit.
                    if registered && now>=next_join {
                        let batch=desired.iter().filter(|channel|!joined.contains(channel)&&retry_at.get(*channel).is_none_or(|at|now>=*at)).take(10).cloned().collect::<Vec<_>>();
                        if batch.is_empty() {continue;}
                        let command=format!("JOIN {}\r\n",batch.iter().map(|c|format!("#{c}")).collect::<Vec<_>>().join(","));
                        let attempted_at=crate::store::now();
                        if changes.is_some() {for channel in &batch {deliver_at!(StreamEvent::RoomJoining(channel.clone()), &attempted_at);}}
                        timeout(Duration::from_secs(5),socket.send(Message::text(command))).await.map_err(|_|IrcError::Timeout)?.map_err(|_|IrcError::Transport)?;
                        for channel in batch { names_deadlines.insert(channel.clone(),Instant::now()+room_timeout); joined.push(channel); }
                        next_join=Instant::now()+join_interval;
                    }
                }
                message=socket.next()=> {
                    let message=message.ok_or(IrcError::Transport)?.map_err(|_|IrcError::Transport)?;
                    last_received=Instant::now();
                    let received_at=crate::store::now();
                    match message {
                        Message::Text(text)=> {
                            for line in buffer.push(text.as_bytes())? {
                                let incoming=parse(&line)?;
                                let room_notice=incoming.command.eq_ignore_ascii_case("NOTICE")&&incoming.params.first().is_some_and(|p|p.starts_with('#'));
                                let own_part=incoming.command.eq_ignore_ascii_case("PART")&&user(incoming.prefix)?==guest;
                                // Once a room is removed, late buffered room messages cannot be
                                // assigned to another room or reopen its closed evidence session.
                                let incoming_room=match incoming.command.to_ascii_uppercase().as_str() {
                                    "353"=>incoming.params.get(2),
                                    "366"=>incoming.params.get(1),
                                    "JOIN"|"PART"=>incoming.params.first(),
                                    "NOTICE" if room_notice=>incoming.params.first(),
                                    _=>None,
                                }.and_then(|raw|raw.strip_prefix('#')).and_then(login);
                                if changes.is_some() && incoming_room.as_ref().is_some_and(|room|!joined.contains(room)) {continue;}
                                if room_notice||own_part {
                                    if room_notice&&!server(incoming.prefix) {return Err(IrcError::Protocol);}
                                    let channel=login(incoming.params.first().and_then(|p|p.strip_prefix('#')).ok_or(IrcError::Protocol)?).ok_or(IrcError::Protocol)?;
                                    if !joined.contains(&channel) {return Err(IrcError::Protocol);}
                                    names_deadlines.remove(&channel);
                                    ready.remove(&channel);
                                    deliver_at!(StreamEvent::RoomUnavailable(channel.clone()), &received_at);
                                    if changes.is_some() {
                                        if !own_part {
                                            timeout(Duration::from_secs(5),socket.send(Message::text(format!("PART #{channel}\r\n")))).await.map_err(|_|IrcError::Timeout)?.map_err(|_|IrcError::Transport)?;
                                        }
                                        joined.retain(|current|current!=&channel);
                                        let count=attempts.entry(channel.clone()).or_default();*count=(*count+1).min(4);
                                        retry_at.insert(channel.clone(),Instant::now()+retry_base.saturating_mul(1<<(*count-1)));
                                        state.rooms.insert(channel,Room::default());
                                    }
                                    continue;
                                }
                                if let Some(reply)=state.line(&line)? {
                                    timeout(Duration::from_secs(5),socket.send(Message::text(reply))).await.map_err(|_|IrcError::Timeout)?.map_err(|_|IrcError::Transport)?;
                                }
                                if state.ready_to_join() {
                                    let _=state.start_join()?;
                                    registered=true;
                                    deliver_at!(StreamEvent::Connected, &received_at);
                                }
                                let message=parse(&line)?;
                                let mut observed=Vec::new();
                                let stamp=received_at.clone();
                                match message.command.to_ascii_uppercase().as_str() {
                                    "353"=> {
                                        let channel=login(message.params[2].trim_start_matches('#')).ok_or(IrcError::Protocol)?;
                                        if !joined.contains(&channel) { return Err(IrcError::Protocol); }
                                        observed=Vec::with_capacity(MAX_RECEIPT_CHUNK);
                                        let mut observed_count=0;
                                        for raw in message.params[3].split_whitespace() {
                                            let account=login(raw.trim_start_matches(['@','+'])).ok_or(IrcError::Protocol)?;
                                            if account!=guest {
                                                observed_count+=1;
                                                if observed_count>MAX_ACCOUNTS { return Err(IrcError::Limit); }
                                                observed.push(crate::store::NewObservation{channel:channel.clone(),account:Some(account),kind:"names".into(),received_at:stamp.clone()});
                                                if observed.len()==MAX_RECEIPT_CHUNK {
                                                    deliver!(StreamEvent::Observations(std::mem::replace(&mut observed,Vec::with_capacity(MAX_RECEIPT_CHUNK))));
                                                }
                                            }
                                        }
                                    }
                                    "366"=> {
                                        let channel=login(message.params[1].trim_start_matches('#')).ok_or(IrcError::Protocol)?;
                                        if !joined.contains(&channel) { return Err(IrcError::Protocol); }
                                        names_deadlines.remove(&channel);
                                        retry_at.remove(&channel);attempts.remove(&channel);
                                        observed=vec![crate::store::NewObservation{channel:channel.clone(),account:None,kind:"names_end".into(),received_at:stamp}];
                                        if ready.insert(channel.clone()) { deliver_at!(StreamEvent::RoomReady(channel), &received_at); }
                                    }
                                    "JOIN"|"PART"=> {
                                        let channel=login(message.params[0].trim_start_matches('#')).ok_or(IrcError::Protocol)?;
                                        if !joined.contains(&channel) { return Err(IrcError::Protocol); }
                                        let account=user(message.prefix)?;
                                        if account!=guest { observed=vec![crate::store::NewObservation{channel,account:Some(account),kind:message.command.to_ascii_lowercase(),received_at:stamp}]; }
                                    }
                                    _=>{}
                                }
                                // The continuous path retains received evidence, not a guessed roster.
                                for room in state.rooms.values_mut() { room.accounts.clear(); room.departed.clear(); room.joins=0; room.parts=0; }
                                if !observed.is_empty() { deliver!(StreamEvent::Observations(observed)); }
                            }
                        }
                        Message::Ping(_)=> { timeout(Duration::from_secs(5),socket.flush()).await.map_err(|_|IrcError::Timeout)?.map_err(|_|IrcError::Transport)?; }
                        Message::Pong(_)=>{}
                        _=>return Err(IrcError::Transport),
                    }
                }
            }
        }
    }.await;
    // Capture the evidence cutoff before potentially slow PART/Close cleanup.
    // Failed admission or cancellation may already have recorded an earlier receipt.
    events.record_end_at(&crate::store::now());
    if !joined.is_empty() {
        let part = format!(
            "PART {}\r\n",
            joined
                .iter()
                .map(|c| format!("#{c}"))
                .collect::<Vec<_>>()
                .join(",")
        );
        let _ = timeout(Duration::from_secs(2), socket.send(Message::text(part))).await;
    }
    let _ = timeout(Duration::from_secs(2), socket.close(None)).await;
    result
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

    fn receipt_rows(start: usize, count: usize) -> Vec<crate::store::NewObservation> {
        (start..start + count)
            .map(|index| crate::store::NewObservation {
                channel: "test".into(),
                account: Some(format!("account{index}")),
                kind: "join".into(),
                received_at: "2026-10-25T00:00:00.000Z".into(),
            })
            .collect()
    }

    #[tokio::test]
    async fn receipt_fifo_and_weights_follow_inflight_ownership_without_retaining_sender() {
        let (sender, mut receiver) = receipt_queue();
        let budget = sender.budget_handle();
        let _end_time = sender.end_time_handle();
        let sink = EventSink::Buffered(sender);
        let (_stop_tx, mut stop_rx) = watch::channel(false);
        for event in [
            StreamEvent::Observations(receipt_rows(0, MAX_RECEIPT_CHUNK)),
            StreamEvent::RoomReady("test".into()),
            StreamEvent::Observations(receipt_rows(MAX_RECEIPT_CHUNK, 1)),
        ] {
            assert!(sink.deliver(event, &mut stop_rx).await.unwrap());
        }
        assert_eq!(budget.available_permits(), RECEIVE_BUFFER_UNITS - 258);
        let first = receiver.recv().await.unwrap();
        assert_eq!(first.received_at(), "2026-10-25T00:00:00.000Z");
        let (first, committed_later) = first.into_parts();
        assert!(matches!(first, StreamEvent::Observations(rows) if rows.len() == 256));
        assert_eq!(committed_later.num_permits(), 256);
        // Retaining a metrics handle must not retain an mpsc Sender.
        drop(sink);
        let (second, control_permit) = receiver.recv().await.unwrap().into_parts();
        assert!(matches!(second, StreamEvent::RoomReady(channel) if channel == "test"));
        assert_eq!(control_permit.num_permits(), 1);
        drop(control_permit);
        let (third, final_permit) = receiver.recv().await.unwrap().into_parts();
        assert!(
            matches!(third, StreamEvent::Observations(rows) if rows[0].account.as_deref() == Some("account256"))
        );
        drop(final_permit);
        assert!(receiver.recv().await.is_none());
        assert_eq!(budget.available_permits(), RECEIVE_BUFFER_UNITS - 256);
        drop(committed_later);
        assert_eq!(budget.available_permits(), RECEIVE_BUFFER_UNITS);
    }

    #[tokio::test(start_paused = true)]
    async fn receipt_full_weight_budget_reports_gap_after_five_seconds_and_recovers() {
        let (sender, receiver) = receipt_queue();
        let budget = sender.budget_handle();
        let end_time = sender.end_time_handle();
        let sink = EventSink::Buffered(sender);
        let (_stop_tx, mut stop_rx) = watch::channel(false);
        for _ in 0..RECEIVE_BUFFER_UNITS / MAX_RECEIPT_CHUNK - 1 {
            assert!(sink
                .deliver(
                    StreamEvent::Observations(receipt_rows(0, MAX_RECEIPT_CHUNK)),
                    &mut stop_rx,
                )
                .await
                .unwrap());
        }
        assert!(sink
            .deliver(
                StreamEvent::Observations(receipt_rows(0, MAX_RECEIPT_CHUNK - 1)),
                &mut stop_rx,
            )
            .await
            .unwrap());
        assert!(sink
            .deliver(StreamEvent::RoomReady("test".into()), &mut stop_rx)
            .await
            .unwrap());
        assert_eq!(budget.available_permits(), 0);
        let started = Instant::now();
        assert_eq!(
            sink.deliver_at(
                StreamEvent::Connected,
                "2026-10-25T01:00:00.000Z",
                &mut stop_rx,
            )
            .await,
            Err(IrcError::Backpressure)
        );
        assert_eq!(started.elapsed(), Duration::from_secs(5));
        // Cleanup and later failures cannot move the first missing receipt forward.
        sink.record_end_at("2026-10-25T01:00:09.000Z");
        assert_eq!(
            end_time.lock().unwrap().as_deref(),
            Some("2026-10-25T01:00:00.000Z")
        );
        drop(receiver);
        assert_eq!(budget.available_permits(), RECEIVE_BUFFER_UNITS);
    }

    #[tokio::test(start_paused = true)]
    async fn receipt_admission_and_channel_send_share_one_deadline() {
        // Smaller independent bounds exercise a send blocked after admission.
        let (events, receiver) = mpsc::channel(1);
        let budget = Arc::new(Semaphore::new(3));
        let sink = EventSink::Buffered(ReceiptSender {
            events,
            budget: Arc::clone(&budget),
            end_time: Arc::new(Mutex::new(None)),
        });
        let (_stop_tx, mut stop_rx) = watch::channel(false);
        assert!(sink
            .deliver(StreamEvent::Connected, &mut stop_rx)
            .await
            .unwrap());
        let inflight = Arc::clone(&budget).acquire_many_owned(2).await.unwrap();
        let started = Instant::now();
        let sending = tokio::spawn(async move {
            sink.deliver(StreamEvent::Observations(receipt_rows(0, 2)), &mut stop_rx)
                .await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(3)).await;
        assert!(!sending.is_finished());
        drop(inflight);
        tokio::task::yield_now().await;
        assert_eq!(budget.available_permits(), 0);
        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(sending.await.unwrap(), Err(IrcError::Backpressure));
        assert_eq!(started.elapsed(), Duration::from_secs(5));
        assert_eq!(budget.available_permits(), 2);
        drop(receiver);
        assert_eq!(budget.available_permits(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn receipt_cancellation_interrupts_admission_and_send_without_leaking_permits() {
        for blocked_in_send in [false, true] {
            let (events, receiver) = mpsc::channel(1);
            let budget = Arc::new(Semaphore::new(3));
            let end_time = Arc::new(Mutex::new(None));
            let sink = EventSink::Buffered(ReceiptSender {
                events,
                budget: Arc::clone(&budget),
                end_time: Arc::clone(&end_time),
            });
            let (stop_tx, mut stop_rx) = watch::channel(false);
            assert!(sink
                .deliver(StreamEvent::Connected, &mut stop_rx)
                .await
                .unwrap());
            let inflight = if blocked_in_send {
                None
            } else {
                Some(Arc::clone(&budget).acquire_many_owned(2).await.unwrap())
            };
            let sending = tokio::spawn(async move {
                sink.deliver(StreamEvent::Observations(receipt_rows(0, 2)), &mut stop_rx)
                    .await
            });
            tokio::task::yield_now().await;
            assert!(!sending.is_finished());
            assert_eq!(budget.available_permits(), 0);
            stop_tx.send(true).unwrap();
            assert_eq!(
                timeout(Duration::from_millis(1), sending)
                    .await
                    .unwrap()
                    .unwrap(),
                Err(IrcError::CancelledEvidence)
            );
            assert_eq!(
                end_time.lock().unwrap().as_deref(),
                Some("2026-10-25T00:00:00.000Z")
            );
            drop(inflight);
            assert_eq!(budget.available_permits(), 2);
            drop(receiver);
            assert_eq!(budget.available_permits(), 3);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn receipt_timestamps_precede_admission_waits_for_observations_and_gap_controls() {
        let (sender, mut receiver) = receipt_queue();
        let budget = sender.budget_handle();
        for (event, supplied_at, expected_at) in [
            (
                StreamEvent::Observations(receipt_rows(0, 1)),
                "2026-10-25T02:00:09.000Z",
                "2026-10-25T00:00:00.000Z",
            ),
            (
                StreamEvent::RoomUnavailable("test".into()),
                "2026-10-25T02:00:00.000Z",
                "2026-10-25T02:00:00.000Z",
            ),
        ] {
            let inflight = Arc::clone(&budget)
                .acquire_many_owned(RECEIVE_BUFFER_UNITS as u32)
                .await
                .unwrap();
            let (_stop_tx, mut stop_rx) = watch::channel(false);
            let sink = EventSink::Buffered(sender.clone());
            let sending =
                tokio::spawn(
                    async move { sink.deliver_at(event, supplied_at, &mut stop_rx).await },
                );
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_secs(3)).await;
            assert!(!sending.is_finished());
            drop(inflight);
            assert!(sending.await.unwrap().unwrap());
            let queued = receiver.recv().await.unwrap();
            assert_eq!(queued.received_at(), expected_at);
            drop(queued);
        }
        assert_eq!(budget.available_permits(), RECEIVE_BUFFER_UNITS);
    }

    #[tokio::test]
    async fn receipt_cancelled_controls_preserve_known_gap_cutoff_and_loss_reason() {
        for (event, expected) in [
            (StreamEvent::RoomJoining("test".into()), Ok(false)),
            (
                StreamEvent::RoomLeft("test".into()),
                Err(IrcError::CancelledEvidence),
            ),
            (
                StreamEvent::RoomUnavailable("test".into()),
                Err(IrcError::CancelledEvidence),
            ),
            (
                StreamEvent::RoomReady("test".into()),
                Err(IrcError::CancelledEvidence),
            ),
        ] {
            let (sender, _receiver) = receipt_queue();
            let end_time = sender.end_time_handle();
            let sink = EventSink::Buffered(sender);
            let (_stop_tx, mut stop_rx) = watch::channel(true);
            assert_eq!(
                sink.deliver_at(event, "2026-10-25T03:00:00.000Z", &mut stop_rx)
                    .await,
                expected
            );
            assert_eq!(
                end_time.lock().unwrap().as_deref(),
                Some("2026-10-25T03:00:00.000Z")
            );
        }
    }

    #[tokio::test]
    async fn receipt_closed_receiver_and_oversize_chunk_release_budget() {
        let (sender, receiver) = receipt_queue();
        let budget = sender.budget_handle();
        let sink = EventSink::Buffered(sender);
        let (_stop_tx, mut stop_rx) = watch::channel(false);
        assert_eq!(
            sink.deliver(
                StreamEvent::Observations(receipt_rows(0, MAX_RECEIPT_CHUNK + 1)),
                &mut stop_rx,
            )
            .await,
            Err(IrcError::Limit)
        );
        assert_eq!(budget.available_permits(), RECEIVE_BUFFER_UNITS);
        drop(receiver);
        assert_eq!(
            sink.deliver(StreamEvent::Connected, &mut stop_rx).await,
            Err(IrcError::Transport)
        );
        assert_eq!(budget.available_permits(), RECEIVE_BUFFER_UNITS);
    }

    #[tokio::test]
    async fn buffered_transport_chunks_names_in_order_and_answers_pings_without_a_consumer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (stop_tx, stop_rx) = watch::channel(false);
        let (_channels_tx, channels_rx) = watch::channel(vec!["test".into()]);
        let (sender, mut receiver) = receipt_queue();
        let budget = sender.budget_handle();
        let end_time = sender.end_time_handle();
        let server_end_time = Arc::clone(&end_time);
        let names_count = MAX_RECEIPT_CHUNK * 2 + 13;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("CAP REQ :twitch.tv/membership\r\n")
            );
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("PASS SCHMOOPIIE\r\n")
            );
            let nick = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let guest = nick.strip_prefix("NICK ").unwrap().trim().to_owned();
            socket.send(Message::text(format!(":tmi.twitch.tv CAP * ACK :twitch.tv/membership\r\n:tmi.twitch.tv 001 {guest} :Welcome\r\n"))).await.unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("JOIN #test\r\n")
            );
            let names = (0..names_count)
                .map(|index| format!("account{index}"))
                .collect::<Vec<_>>()
                .join(" ");
            socket.send(Message::text(format!(":tmi.twitch.tv 353 {guest} = #test :{guest} {names}\r\n:tmi.twitch.tv 366 {guest} #test :End\r\n:alice!alice@alice.tmi.twitch.tv JOIN #test\r\n"))).await.unwrap();
            socket
                .send(Message::Ping(vec![1, 2, 3].into()))
                .await
                .unwrap();
            socket
                .send(Message::text("PING :buffered\r\n"))
                .await
                .unwrap();
            assert_eq!(
                timeout(Duration::from_secs(1), socket.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
                Message::Pong(vec![1, 2, 3].into())
            );
            assert_eq!(
                timeout(Duration::from_secs(1), socket.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
                Message::text("PONG :buffered\r\n")
            );
            stop_tx.send(true).unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("PART #test\r\n")
            );
            assert!(server_end_time.lock().unwrap().is_some());
            assert!(matches!(
                socket.next().await.unwrap().unwrap(),
                Message::Close(_)
            ));
        });
        timeout(
            Duration::from_secs(5),
            stream_dynamic_configured(
                channels_rx,
                sender,
                stop_rx,
                &endpoint,
                Duration::from_secs(2),
                Duration::from_millis(10),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        server.await.unwrap();
        // No receipt has been consumed before both PING responses and cleanup.
        assert_eq!(
            budget.available_permits(),
            RECEIVE_BUFFER_UNITS - names_count - 5
        );
        let mut order = Vec::new();
        let mut accounts = Vec::new();
        while let Some(queued) = receiver.recv().await {
            let received_at = queued.received_at().to_owned();
            let (event, permit) = queued.into_parts();
            match event {
                StreamEvent::Connected => order.push(("connected".to_owned(), 1)),
                StreamEvent::RoomJoining(_) => order.push(("joining".to_owned(), 1)),
                StreamEvent::RoomReady(_) => order.push(("ready".to_owned(), 1)),
                StreamEvent::Observations(rows) => {
                    assert_eq!(permit.num_permits(), rows.len());
                    assert!(rows.len() <= MAX_RECEIPT_CHUNK);
                    assert!(rows.iter().all(|row| row.received_at == received_at));
                    order.push((rows[0].kind.clone(), rows.len()));
                    accounts.extend(rows.into_iter().filter_map(|row| row.account));
                }
                unexpected => panic!("unexpected event: {unexpected:?}"),
            }
        }
        assert_eq!(
            order,
            vec![
                ("connected", 1),
                ("joining", 1),
                ("names", 256),
                ("names", 256),
                ("names", 13),
                ("ready", 1),
                ("names_end", 1),
                ("join", 1)
            ]
            .into_iter()
            .map(|(kind, count)| (kind.to_owned(), count))
            .collect::<Vec<_>>()
        );
        let mut expected = (0..names_count)
            .map(|index| format!("account{index}"))
            .collect::<Vec<_>>();
        expected.push("alice".into());
        assert_eq!(accounts, expected);
        assert_eq!(budget.available_permits(), RECEIVE_BUFFER_UNITS);
        assert!(end_time.lock().unwrap().is_some());
    }

    #[tokio::test]
    async fn dynamic_room_retry_and_configuration_keep_healthy_room_connected() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let (channels_tx, channels_rx) =
            tokio::sync::watch::channel(vec!["healthy".into(), "missing".into()]);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = socket.next().await;
            let _ = socket.next().await;
            let nick = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let guest = nick.strip_prefix("NICK ").unwrap().trim().to_owned();
            socket.send(Message::text(format!(":tmi.twitch.tv CAP * ACK :twitch.tv/membership\r\n:tmi.twitch.tv 001 {guest} :Welcome\r\n"))).await.unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("JOIN #healthy,#missing\r\n")
            );
            socket.send(Message::text(format!(":tmi.twitch.tv 366 {guest} #healthy :End\r\n:alice!alice@alice.tmi.twitch.tv JOIN #healthy\r\n:tmi.twitch.tv NOTICE #missing :Unavailable\r\n"))).await.unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("PART #missing\r\n")
            );
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("JOIN #missing\r\n")
            );
            socket.send(Message::text(format!(":tmi.twitch.tv 366 {guest} #missing :End\r\n:bob!bob@bob.tmi.twitch.tv JOIN #healthy\r\n"))).await.unwrap();
            channels_tx
                .send(vec!["healthy".into(), "new".into()])
                .unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("PART #missing\r\n")
            );
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("JOIN #new\r\n")
            );
            socket.send(Message::text(format!(":tmi.twitch.tv 353 {guest} = #missing :lateaccount\r\n:tmi.twitch.tv 366 {guest} #new :End\r\nPING :still-healthy\r\n"))).await.unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("PONG :still-healthy\r\n")
            );
            stop_tx.send(true).unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("PART #healthy,#new\r\n")
            );
            assert!(matches!(
                socket.next().await.unwrap().unwrap(),
                Message::Close(_)
            ));
        });
        let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move {
            let mut healthy_joins = 0;
            let mut missing_joins = 0;
            let mut missing_gaps = 0;
            let mut healthy_accounts = Vec::new();
            while let Some(event) = receiver.recv().await {
                match event {
                    StreamEvent::RoomJoining(channel) if channel == "healthy" => healthy_joins += 1,
                    StreamEvent::RoomJoining(channel) if channel == "missing" => missing_joins += 1,
                    StreamEvent::RoomUnavailable(channel) if channel == "missing" => {
                        missing_gaps += 1
                    }
                    StreamEvent::Observations(events) => {
                        for event in events {
                            if let Some(account) = event.account {
                                assert_ne!(account, "lateaccount");
                                if event.channel == "healthy" {
                                    healthy_accounts.push(account);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            assert_eq!(healthy_joins, 1);
            assert_eq!(missing_joins, 2);
            assert_eq!(missing_gaps, 1);
            assert_eq!(healthy_accounts, vec!["alice", "bob"]);
        });
        timeout(
            Duration::from_secs(8),
            stream_managed(
                vec!["healthy".into(), "missing".into()],
                EventSink::Legacy(sender),
                stop_rx,
                &endpoint,
                Duration::from_secs(2),
                Some(channels_rx),
                Duration::from_millis(100),
                Duration::from_millis(100),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        server.await.unwrap();
        drain.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn continuous_delivery_stall_is_bounded_and_explicit() {
        let (sender, _receiver) = tokio::sync::mpsc::channel(1);
        sender.send(StreamEvent::Connected).await.unwrap();
        let (_stop_tx, mut stop_rx) = watch::channel(false);
        assert_eq!(
            EventSink::Legacy(sender)
                .deliver(StreamEvent::Connected, &mut stop_rx)
                .await,
            Err(IrcError::Backpressure)
        );
    }

    #[tokio::test]
    async fn unavailable_room_does_not_disconnect_healthy_membership() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = socket.next().await;
            let _ = socket.next().await;
            let nick = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let guest = nick.strip_prefix("NICK ").unwrap().trim().to_owned();
            socket.send(Message::text(format!(":tmi.twitch.tv CAP * ACK :twitch.tv/membership\r\n:tmi.twitch.tv 001 {guest} :Welcome\r\n"))).await.unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("JOIN #healthy,#missing\r\n")
            );
            socket
                .send(Message::text(format!(
                    ":tmi.twitch.tv 366 {guest} #healthy :End\r\n"
                )))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(400)).await;
            socket
                .send(Message::text(
                    ":alice!alice@alice.tmi.twitch.tv JOIN #healthy\r\nPING :still-connected\r\n",
                ))
                .await
                .unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("PONG :still-connected\r\n")
            );
            stop_tx.send(true).unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("PART #healthy,#missing\r\n")
            );
            assert!(matches!(
                socket.next().await.unwrap().unwrap(),
                Message::Close(_)
            ));
        });
        let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move {
            let mut unavailable = false;
            let mut healthy = false;
            while let Some(event) = receiver.recv().await {
                match event {
                    StreamEvent::RoomUnavailable(channel) => {
                        assert_eq!(channel, "missing");
                        unavailable = true;
                    }
                    StreamEvent::Observations(rows) => {
                        healthy |= rows.iter().any(|e| e.account.as_deref() == Some("alice"));
                    }
                    _ => {}
                }
            }
            assert!(unavailable && healthy);
        });
        timeout(
            Duration::from_secs(5),
            stream_configured(
                vec!["healthy".into(), "missing".into()],
                sender,
                stop_rx,
                &endpoint,
                Duration::from_millis(200),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        server.await.unwrap();
        drain.await.unwrap();
    }

    #[tokio::test]
    async fn continuous_join_batches_count_channels_and_keep_processing_ping() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("CAP REQ :twitch.tv/membership\r\n")
            );
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("PASS SCHMOOPIIE\r\n")
            );
            let nick = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let guest = nick.strip_prefix("NICK ").unwrap().trim().to_owned();
            socket.send(Message::text(format!(":tmi.twitch.tv CAP * ACK :twitch.tv/membership\r\n:tmi.twitch.tv 001 {guest} :Welcome\r\n"))).await.unwrap();
            let joined = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let targets = joined.strip_prefix("JOIN ").unwrap().trim();
            assert_eq!(targets.split(',').count(), 10);
            assert!(timeout(Duration::from_millis(100), socket.next())
                .await
                .is_err());
            socket
                .send(Message::text("PING :during-paced-joins\r\n"))
                .await
                .unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("PONG :during-paced-joins\r\n")
            );
            stop_tx.send(true).unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text(format!("PART {targets}\r\n"))
            );
            assert!(matches!(
                socket.next().await.unwrap().unwrap(),
                Message::Close(_)
            ));
        });
        let (events_tx, mut events_rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while events_rx.recv().await.is_some() {} });
        let channels = (0..50).map(|n| format!("channel{n:02}")).collect();
        timeout(
            Duration::from_secs(5),
            stream_endpoint(channels, events_tx, stop_rx, &endpoint),
        )
        .await
        .unwrap()
        .unwrap();
        server.await.unwrap();
        drain.await.unwrap();
        assert!(
            normalize_channels(&(0..4).map(|n| format!("channel{n}")).collect::<Vec<_>>()).is_err()
        );
    }

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
