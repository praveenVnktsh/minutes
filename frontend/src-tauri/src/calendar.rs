//! Calendar subscription (ICS/iCal) integration.
//!
//! Privacy-first alternative to OAuth: the user pastes the "secret address in
//! iCal format" from Google Calendar (or any other provider that exposes an
//! ICS feed), and we poll it over HTTPS. No OAuth, no client credentials, no
//! Google review. The feed URL is treated as a bearer token and is never
//! logged.
//!
//! Responsibilities:
//! - persist the subscription config (`calendar-settings.json`)
//! - fetch + parse the feed, expanding `RRULE` recurrences and applying
//!   `RECURRENCE-ID` overrides / cancellations
//! - cache the expanded upcoming events in memory
//! - run a background worker that refreshes the feed and fires meeting
//!   reminders through the existing notification manager

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Duration, TimeZone, Utc};
use futures_util::StreamExt;
use icalendar::{Calendar, Component, DatePerhapsTime, Event, EventLike, EventStatus};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_store::StoreExt;

use crate::notifications::commands::NotificationManagerState;
use crate::notifications::settings::ConsentManager;

const STORE_FILE: &str = "calendar-settings.json";
const STORE_KEY: &str = "calendar";

/// Hard cap on expanded occurrences per recurring series, to keep a malformed
/// or extremely long rule from generating unbounded work.
const MAX_OCCURRENCES_PER_SERIES: u16 = 512;

/// How long after the scheduled reminder time a reminder may still be shown.
/// Must be larger than the worker tick so a reminder is always captured exactly
/// once even if a tick is delayed.
const REMINDER_GRACE_SECONDS: i64 = 150;

const WORKER_TICK_SECONDS: u64 = 60;

/// Upper bound on a feed body. A runaway or hostile feed must not be able to
/// exhaust memory; real calendars are well under this.
const MAX_FEED_BYTES: usize = 16 * 1024 * 1024;

const DEFAULT_REFRESH_MINUTES: u64 = 15;
const DEFAULT_LOOKAHEAD_DAYS: u32 = 7;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

fn default_refresh_minutes() -> u64 {
    DEFAULT_REFRESH_MINUTES
}

fn default_lookahead_days() -> u32 {
    DEFAULT_LOOKAHEAD_DAYS
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarConfig {
    /// Whether the background sync and reminder worker is active.
    pub enabled: bool,
    /// ICS/iCal subscription URL (https:// or webcal://).
    #[serde(default)]
    pub url: String,
    /// How often to re-poll the feed, in minutes.
    #[serde(default = "default_refresh_minutes")]
    pub refresh_minutes: u64,
    /// How many days ahead to expand and keep in the cache.
    #[serde(default = "default_lookahead_days")]
    pub lookahead_days: u32,
    /// Whether to raise meeting reminders for upcoming events.
    #[serde(default = "default_true")]
    pub remind: bool,
}

impl Default for CalendarConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            refresh_minutes: DEFAULT_REFRESH_MINUTES,
            lookahead_days: DEFAULT_LOOKAHEAD_DAYS,
            remind: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Data exposed to the frontend
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEventOut {
    pub uid: String,
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: Option<DateTime<Utc>>,
    pub location: Option<String>,
    pub all_day: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarSnapshot {
    pub fetched_at: DateTime<Utc>,
    pub events: Vec<CalendarEventOut>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FeedState {
    /// Normalized URL the cached body came from.
    url: String,
    etag: Option<String>,
    /// Raw ICS body, kept so a `304 Not Modified` can still be re-expanded for
    /// a sliding time window.
    raw: String,
}

#[derive(Default)]
struct Override {
    start: Option<DateTime<Utc>>,
    duration: Option<Duration>,
    title: Option<String>,
    cancelled: bool,
}

static SNAPSHOT: LazyLock<Mutex<Option<CalendarSnapshot>>> = LazyLock::new(|| Mutex::new(None));
static FEED_STATE: LazyLock<Mutex<FeedState>> = LazyLock::new(|| Mutex::new(FeedState::default()));
static NOTIFIED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

// ---------------------------------------------------------------------------
// URL helpers
// ---------------------------------------------------------------------------

/// Accepts `webcal://` (as copied from most calendar apps) and normalizes it to
/// an `https://` URL. Rejects anything that is not http(s).
pub fn normalize_feed_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Calendar URL is empty".to_string());
    }

    let rewritten = if let Some(rest) = trimmed.strip_prefix("webcal://") {
        format!("https://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("webcals://") {
        format!("https://{rest}")
    } else {
        trimmed.to_string()
    };

    let parsed =
        url::Url::parse(&rewritten).map_err(|_| "Calendar URL is not a valid URL".to_string())?;
    let is_localhost = parsed.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
    });
    match parsed.scheme() {
        "https" => Ok(parsed.to_string()),
        "http" if is_localhost => Ok(parsed.to_string()),
        "http" => Err(
            "Calendar URL must use HTTPS (plain HTTP is only allowed for localhost)".to_string(),
        ),
        other => Err(format!("Unsupported calendar URL scheme: {other}")),
    }
}

// ---------------------------------------------------------------------------
// Config persistence
// ---------------------------------------------------------------------------

fn load_config<R: Runtime>(app: &AppHandle<R>) -> Result<CalendarConfig, String> {
    let store = app
        .store(STORE_FILE)
        .map_err(|error| format!("Failed to open calendar settings: {error}"))?;

    match store.get(STORE_KEY) {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|error| format!("Failed to read calendar settings: {error}")),
        None => Ok(CalendarConfig::default()),
    }
}

fn save_config<R: Runtime>(app: &AppHandle<R>, config: &CalendarConfig) -> Result<(), String> {
    let store = app
        .store(STORE_FILE)
        .map_err(|error| format!("Failed to open calendar settings: {error}"))?;
    store.set(
        STORE_KEY,
        serde_json::to_value(config)
            .map_err(|error| format!("Failed to serialize calendar settings: {error}"))?,
    );
    store
        .save()
        .map_err(|error| format!("Failed to save calendar settings: {error}"))
}

fn validate_config(config: &CalendarConfig) -> Result<(), String> {
    if config.enabled || !config.url.trim().is_empty() {
        normalize_feed_url(&config.url)?;
    }
    if config.refresh_minutes == 0 || config.refresh_minutes > 1440 {
        return Err("Refresh interval must be between 1 and 1440 minutes".to_string());
    }
    if config.lookahead_days == 0 || config.lookahead_days > 60 {
        return Err("Look-ahead must be between 1 and 60 days".to_string());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

fn build_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(StdDuration::from_secs(20))
        .user_agent(concat!(
            "Minutes/",
            env!("CARGO_PKG_VERSION"),
            " (calendar-sync)"
        ))
        .build()
        .map_err(|error| format!("Failed to create HTTP client: {error}"))
}

enum FetchOutcome {
    NotModified,
    Body { text: String, etag: Option<String> },
}

async fn fetch_feed(
    client: &reqwest::Client,
    url: &str,
    etag: Option<&str>,
) -> Result<FetchOutcome, String> {
    let mut request = client
        .get(url)
        .header("accept", "text/calendar, text/plain;q=0.9, */*;q=0.5");
    if let Some(tag) = etag {
        request = request.header("if-none-match", tag);
    }

    let response = request
        .send()
        .await
        .map_err(|error| format!("Could not reach the calendar URL: {error}"))?;

    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(FetchOutcome::NotModified);
    }
    if !response.status().is_success() {
        // Deliberately omit the URL: it is a bearer token.
        return Err(format!("Calendar feed returned HTTP {}", response.status()));
    }

    let new_etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);

    if response
        .content_length()
        .is_some_and(|length| length > MAX_FEED_BYTES as u64)
    {
        return Err("Calendar feed is larger than the 16 MB limit".to_string());
    }

    // Read in chunks so a lying/absent Content-Length cannot bypass the cap.
    let mut buffer: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("Could not read calendar response: {error}"))?;
        if buffer.len() + chunk.len() > MAX_FEED_BYTES {
            return Err("Calendar feed is larger than the 16 MB limit".to_string());
        }
        buffer.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&buffer).into_owned();

    Ok(FetchOutcome::Body {
        text,
        etag: new_etag,
    })
}

// ---------------------------------------------------------------------------
// Parsing / recurrence expansion
// ---------------------------------------------------------------------------

fn to_utc(value: &DatePerhapsTime) -> Option<DateTime<Utc>> {
    match value {
        DatePerhapsTime::DateTime(calendar_date_time) => calendar_date_time.try_into_utc(),
        DatePerhapsTime::Date(date) => date
            .and_hms_opt(0, 0, 0)
            .map(|naive| Utc.from_utc_datetime(&naive)),
    }
}

fn event_duration(event: &Event) -> Option<Duration> {
    let start = event.get_start().as_ref().and_then(to_utc)?;
    let end = event.get_end().as_ref().and_then(to_utc)?;
    let duration = end.signed_duration_since(start);
    (duration > Duration::zero()).then_some(duration)
}

fn is_recurring(event: &Event) -> bool {
    event.property_value("RRULE").is_some() || event.property_value("RDATE").is_some()
}

fn is_cancelled(event: &Event) -> bool {
    matches!(event.get_status(), Some(EventStatus::Cancelled))
}

/// Parse an ICS document and expand it into concrete occurrences within
/// `[window_start, window_end]`, applying recurrence overrides and
/// cancellations.
pub fn expand_ics(
    body: &str,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
) -> Result<Vec<CalendarEventOut>, String> {
    let calendar: Calendar = body
        .parse()
        .map_err(|error| format!("Calendar feed is not valid iCalendar data: {error}"))?;

    let raw: Vec<_> = calendar.calendar_events().collect();

    // First pass: collect overrides and cancellations keyed by
    // (UID, original occurrence start).
    let mut overrides: HashMap<(String, i64), Override> = HashMap::new();
    let mut base_indices: Vec<usize> = Vec::new();

    for (index, calendar_event) in raw.iter().enumerate() {
        let event = calendar_event.event();
        let uid = event.get_uid().unwrap_or_default().to_string();

        match event.get_recurrence_id().as_ref().and_then(to_utc) {
            Some(recurrence_id) => {
                overrides.insert(
                    (uid, recurrence_id.timestamp()),
                    Override {
                        start: event.get_start().as_ref().and_then(to_utc),
                        duration: event_duration(event),
                        title: event.get_summary().map(str::to_string),
                        cancelled: is_cancelled(event),
                    },
                );
            }
            None if !is_cancelled(event) => base_indices.push(index),
            None => {}
        }
    }

    let mut output: Vec<CalendarEventOut> = Vec::new();

    for index in base_indices {
        let calendar_event = &raw[index];
        let event = calendar_event.event();

        let uid = event.get_uid().unwrap_or_default().to_string();
        let base_title = event.get_summary().unwrap_or("Untitled event").to_string();
        let location = event.get_location().map(str::to_string);
        let all_day = matches!(event.get_start(), Some(DatePerhapsTime::Date(_)));
        let base_duration = event_duration(event);
        let base_start = event.get_start().as_ref().and_then(to_utc);

        if is_recurring(event) {
            let recurrences = match calendar_event.get_recurrence() {
                Ok(set) => set,
                Err(error) => {
                    log::warn!("Skipping recurring event {uid}: {error}");
                    continue;
                }
            };

            let timezone = recurrences.get_dt_start().timezone();
            let after = timezone.from_utc_datetime(&window_start.naive_utc());
            let before = timezone.from_utc_datetime(&window_end.naive_utc());
            let result = recurrences
                .after(after)
                .before(before)
                .all(MAX_OCCURRENCES_PER_SERIES);

            for occurrence in result.dates {
                let occurrence_start = occurrence.with_timezone(&Utc);
                let key = (uid.clone(), occurrence_start.timestamp());

                if let Some(override_entry) = overrides.get(&key) {
                    if override_entry.cancelled {
                        continue;
                    }
                    let start = override_entry.start.unwrap_or(occurrence_start);
                    let duration = override_entry.duration.or(base_duration);
                    output.push(CalendarEventOut {
                        uid: uid.clone(),
                        title: override_entry
                            .title
                            .clone()
                            .unwrap_or_else(|| base_title.clone()),
                        start,
                        end: duration.map(|value| start + value),
                        location: location.clone(),
                        all_day,
                    });
                } else {
                    output.push(CalendarEventOut {
                        uid: uid.clone(),
                        title: base_title.clone(),
                        start: occurrence_start,
                        end: base_duration.map(|value| occurrence_start + value),
                        location: location.clone(),
                        all_day,
                    });
                }
            }
        } else if let Some(start) = base_start {
            if overrides
                .get(&(uid.clone(), start.timestamp()))
                .is_some_and(|entry| entry.cancelled)
            {
                continue;
            }
            output.push(CalendarEventOut {
                uid: uid.clone(),
                title: base_title.clone(),
                start,
                end: base_duration.map(|value| start + value),
                location: location.clone(),
                all_day,
            });
        }
    }

    output.sort_by(|a, b| a.start.cmp(&b.start));
    Ok(output)
}

// ---------------------------------------------------------------------------
// Refresh
// ---------------------------------------------------------------------------

fn current_window(config: &CalendarConfig) -> (DateTime<Utc>, DateTime<Utc>) {
    let now = Utc::now();
    (
        now - Duration::hours(1),
        now + Duration::days(i64::from(config.lookahead_days)),
    )
}

async fn refresh_feed(config: &CalendarConfig) -> Result<CalendarSnapshot, String> {
    let url = normalize_feed_url(&config.url)?;
    let client = build_http_client()?;

    let cached = {
        let state = FEED_STATE.lock().unwrap();
        if state.url == url {
            Some((state.etag.clone(), state.raw.clone()))
        } else {
            None
        }
    };
    let (cached_etag, cached_raw) = cached.unwrap_or((None, String::new()));

    let outcome = fetch_feed(&client, &url, cached_etag.as_deref()).await?;

    let (raw, etag) = match outcome {
        FetchOutcome::NotModified if !cached_raw.is_empty() => (cached_raw, cached_etag),
        FetchOutcome::NotModified => match fetch_feed(&client, &url, None).await? {
            FetchOutcome::Body { text, etag } => (text, etag),
            FetchOutcome::NotModified => {
                return Err(
                    "Calendar feed reported no changes but no cached copy exists".to_string(),
                )
            }
        },
        FetchOutcome::Body { text, etag } => (text, etag),
    };

    let (window_start, window_end) = current_window(config);
    let events = expand_ics(&raw, window_start, window_end)?;

    let snapshot = CalendarSnapshot {
        fetched_at: Utc::now(),
        events,
    };

    *SNAPSHOT.lock().unwrap() = Some(snapshot.clone());
    {
        let mut state = FEED_STATE.lock().unwrap();
        state.url = url;
        state.etag = etag;
        state.raw = raw;
    }

    Ok(snapshot)
}

// ---------------------------------------------------------------------------
// Reminders
// ---------------------------------------------------------------------------

fn prune_notified(now: DateTime<Utc>) {
    let mut notified = NOTIFIED.lock().unwrap();
    if notified.len() < 1024 {
        return;
    }
    let cutoff = now.timestamp() - 86_400;
    notified.retain(|key| {
        key.split('|')
            .nth(1)
            .and_then(|value| value.parse::<i64>().ok())
            .is_some_and(|timestamp| timestamp > cutoff)
    });
}

async fn maybe_remind<R: Runtime>(app: &AppHandle<R>) {
    let offsets = match ConsentManager::new(app.clone()) {
        Ok(manager) => manager
            .load_settings()
            .await
            .map(|settings| settings.notification_preferences.meeting_reminder_minutes)
            .unwrap_or_default(),
        Err(_) => return,
    };
    if offsets.is_empty() {
        return;
    }

    let snapshot = match SNAPSHOT.lock().unwrap().clone() {
        Some(snapshot) => snapshot,
        None => return,
    };

    let now = Utc::now();
    let manager_state = app.state::<NotificationManagerState<R>>();

    for event in snapshot.events.iter().filter(|event| !event.all_day) {
        for &offset in &offsets {
            if offset == 0 {
                continue;
            }
            let remind_at = event.start - Duration::minutes(offset as i64);
            let elapsed = now.signed_duration_since(remind_at);
            if elapsed < Duration::zero() || elapsed >= Duration::seconds(REMINDER_GRACE_SECONDS) {
                continue;
            }

            let key = format!("{}|{}|{}", event.uid, event.start.timestamp(), offset);
            if !NOTIFIED.lock().unwrap().insert(key) {
                continue;
            }

            let manager_lock = manager_state.read().await;
            if let Some(manager) = manager_lock.as_ref() {
                if let Err(error) = manager
                    .show_meeting_reminder(offset, Some(event.title.clone()))
                    .await
                {
                    log::warn!("Failed to show calendar reminder: {error}");
                }
            }
        }
    }

    prune_notified(now);
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

pub fn init_worker<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(StdDuration::from_secs(WORKER_TICK_SECONDS));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_fetch: Option<Instant> = None;

        loop {
            interval.tick().await;

            let config = match load_config(&app) {
                Ok(config) => config,
                Err(error) => {
                    log::warn!("Calendar settings unavailable: {error}");
                    continue;
                }
            };

            if !config.enabled || config.url.trim().is_empty() {
                continue;
            }

            let due = match last_fetch {
                None => true,
                Some(at) => {
                    at.elapsed() >= StdDuration::from_secs(config.refresh_minutes.max(1) * 60)
                }
            };

            if due {
                match refresh_feed(&config).await {
                    Ok(snapshot) => {
                        log::info!("Calendar synced: {} upcoming events", snapshot.events.len());
                        let _ = app.emit("calendar-events-updated", &snapshot);
                        last_fetch = Some(Instant::now());
                    }
                    Err(error) => log::warn!("Calendar sync failed: {error}"),
                }
            }

            if config.remind {
                maybe_remind(&app).await;
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_calendar_config<R: Runtime>(app: AppHandle<R>) -> Result<CalendarConfig, String> {
    load_config(&app)
}

#[tauri::command]
pub async fn set_calendar_config<R: Runtime>(
    app: AppHandle<R>,
    config: CalendarConfig,
) -> Result<(), String> {
    validate_config(&config)?;
    save_config(&app, &config)?;

    // The cached body and ETag are tied to the previous URL; drop them so the
    // next fetch is unconditional.
    *FEED_STATE.lock().unwrap() = FeedState::default();

    if config.enabled && !config.url.trim().is_empty() {
        let app_for_refresh = app.clone();
        tauri::async_runtime::spawn(async move {
            match refresh_feed(&config).await {
                Ok(snapshot) => {
                    let _ = app_for_refresh.emit("calendar-events-updated", &snapshot);
                }
                Err(error) => log::warn!("Calendar refresh after save failed: {error}"),
            }
        });
    }

    Ok(())
}

#[tauri::command]
pub async fn test_calendar_feed<R: Runtime>(
    app: AppHandle<R>,
    url: Option<String>,
) -> Result<Vec<CalendarEventOut>, String> {
    let candidate = match url {
        Some(value) if !value.trim().is_empty() => value,
        _ => load_config(&app)?.url,
    };
    let normalized = normalize_feed_url(&candidate)?;

    let client = build_http_client()?;
    let outcome = fetch_feed(&client, &normalized, None).await?;
    let text = match outcome {
        FetchOutcome::Body { text, .. } => text,
        FetchOutcome::NotModified => return Err("Calendar feed returned no content".to_string()),
    };

    let now = Utc::now();
    let mut events = expand_ics(&text, now - Duration::hours(1), now + Duration::days(14))?;
    events.truncate(25);
    Ok(events)
}

#[tauri::command]
pub async fn refresh_calendar_now<R: Runtime>(
    app: AppHandle<R>,
) -> Result<CalendarSnapshot, String> {
    let config = load_config(&app)?;
    if config.url.trim().is_empty() {
        return Err("No calendar URL is configured".to_string());
    }
    if !config.enabled {
        return Err("Calendar sync is disabled".to_string());
    }

    let snapshot = refresh_feed(&config).await?;
    let _ = app.emit("calendar-events-updated", &snapshot);
    Ok(snapshot)
}

#[tauri::command]
pub async fn get_calendar_events() -> Result<Option<CalendarSnapshot>, String> {
    Ok(SNAPSHOT.lock().unwrap().clone())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//Minutes Test//EN\r\n\
BEGIN:VEVENT\r\n\
UID:single@example.com\r\n\
DTSTAMP:20260101T000000Z\r\n\
DTSTART:20260115T150000Z\r\n\
DTEND:20260115T160000Z\r\n\
SUMMARY:Single meeting\r\n\
LOCATION:Room 1\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:daily@example.com\r\n\
DTSTAMP:20260101T000000Z\r\n\
DTSTART:20260110T090000Z\r\n\
DTEND:20260110T093000Z\r\n\
SUMMARY:Daily standup\r\n\
RRULE:FREQ=DAILY;COUNT=10\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:weekly@example.com\r\n\
DTSTAMP:20260101T000000Z\r\n\
DTSTART:20260105T140000Z\r\n\
DTEND:20260105T150000Z\r\n\
SUMMARY:Weekly sync\r\n\
RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=6\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:weekly@example.com\r\n\
DTSTAMP:20260101T000000Z\r\n\
RECURRENCE-ID:20260112T140000Z\r\n\
DTSTART:20260112T170000Z\r\n\
DTEND:20260112T180000Z\r\n\
SUMMARY:Weekly sync moved\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:cancelled@example.com\r\n\
DTSTAMP:20260101T000000Z\r\n\
DTSTART:20260114T100000Z\r\n\
DTEND:20260114T110000Z\r\n\
SUMMARY:Cancelled series\r\n\
RRULE:FREQ=DAILY;COUNT=5\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:cancelled@example.com\r\n\
DTSTAMP:20260101T000000Z\r\n\
RECURRENCE-ID:20260115T100000Z\r\n\
STATUS:CANCELLED\r\n\
DTSTART:20260115T100000Z\r\n\
DTEND:20260115T110000Z\r\n\
SUMMARY:Cancelled series\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:allday@example.com\r\n\
DTSTAMP:20260101T000000Z\r\n\
DTSTART;VALUE=DATE:20260120\r\n\
DTEND;VALUE=DATE:20260121\r\n\
SUMMARY:Company holiday\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    fn at(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap()
    }

    #[test]
    fn normalizes_webcal_and_https_urls() {
        assert_eq!(
            normalize_feed_url("webcal://calendar.google.com/x/basic.ics").unwrap(),
            "https://calendar.google.com/x/basic.ics"
        );
        assert!(normalize_feed_url("https://example.com/feed.ics").is_ok());
        // Plain HTTP would send the bearer token in the clear.
        assert!(normalize_feed_url("http://example.com/feed.ics").is_err());
        assert!(normalize_feed_url("http://localhost:8080/feed.ics").is_ok());
        assert!(normalize_feed_url("ftp://example.com/feed.ics").is_err());
        assert!(normalize_feed_url("   ").is_err());
    }

    #[test]
    fn includes_single_events_in_window() {
        let events = expand_ics(SAMPLE, at(2026, 1, 15), at(2026, 1, 16)).unwrap();
        assert!(events.iter().any(|event| event.uid == "single@example.com"));
        let single = events
            .iter()
            .find(|event| event.uid == "single@example.com")
            .unwrap();
        assert_eq!(single.title, "Single meeting");
        assert_eq!(single.location.as_deref(), Some("Room 1"));
        assert_eq!(single.end.unwrap() - single.start, Duration::hours(1));
    }

    #[test]
    fn expands_daily_recurrence() {
        let events = expand_ics(SAMPLE, at(2026, 1, 10), at(2026, 1, 13)).unwrap();
        let daily: Vec<_> = events
            .iter()
            .filter(|event| event.uid == "daily@example.com")
            .collect();
        assert_eq!(daily.len(), 3);
        assert_eq!(
            daily[0].start,
            Utc.with_ymd_and_hms(2026, 1, 10, 9, 0, 0).unwrap()
        );
        assert_eq!(
            daily[2].start,
            Utc.with_ymd_and_hms(2026, 1, 12, 9, 0, 0).unwrap()
        );
    }

    #[test]
    fn applies_recurrence_override() {
        let events = expand_ics(SAMPLE, at(2026, 1, 12), at(2026, 1, 13)).unwrap();
        let weekly: Vec<_> = events
            .iter()
            .filter(|event| event.uid == "weekly@example.com")
            .collect();
        assert_eq!(weekly.len(), 1);
        assert_eq!(weekly[0].title, "Weekly sync moved");
        assert_eq!(
            weekly[0].start,
            Utc.with_ymd_and_hms(2026, 1, 12, 17, 0, 0).unwrap()
        );
    }

    #[test]
    fn drops_cancelled_occurrences() {
        let events = expand_ics(SAMPLE, at(2026, 1, 14), at(2026, 1, 16)).unwrap();
        let cancelled: Vec<_> = events
            .iter()
            .filter(|event| event.uid == "cancelled@example.com")
            .collect();
        // 01-14 is kept, 01-15 is cancelled.
        assert_eq!(cancelled.len(), 1);
        assert_eq!(
            cancelled[0].start,
            Utc.with_ymd_and_hms(2026, 1, 14, 10, 0, 0).unwrap()
        );
    }

    #[test]
    fn flags_all_day_events() {
        let events = expand_ics(SAMPLE, at(2026, 1, 20), at(2026, 1, 21)).unwrap();
        let holiday = events
            .iter()
            .find(|event| event.uid == "allday@example.com")
            .unwrap();
        assert!(holiday.all_day);
        assert_eq!(holiday.start, at(2026, 1, 20));
    }

    #[test]
    fn rejects_invalid_feed() {
        assert!(expand_ics("not a calendar", at(2026, 1, 1), at(2026, 1, 2)).is_err());
    }
}
