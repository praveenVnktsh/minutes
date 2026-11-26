//! Links a new recording's meeting to the calendar invite it belongs to: the
//! invite's guests become people (`source: calendar`) and the meeting's
//! attendees.
//!
//! Called once a recording's meeting row exists and is bound to the
//! recording (`meeting_activity::bind_active_recording_meeting`). Imported
//! recordings never come through here. Nothing in this module may fail a
//! recording: [`spawn_for_recording`] logs and moves on.

use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use super::{PersonEdit, PersonInput, SOURCE_CALENDAR};
use crate::calendar::{CalendarAttendee, CalendarEventOut, CalendarSnapshot};

/// A recording that starts this long before an event still belongs to it.
const EARLY_START_MINUTES: i64 = 10;

/// An event with a start but no end is assumed to last this long.
const ASSUMED_LENGTH_MINUTES: i64 = 30;

fn same_title(a: &str, b: &str) -> bool {
    let a = a.trim();
    !a.is_empty() && a.to_lowercase() == b.trim().to_lowercase()
}

/// The invite a meeting starting at `now` belongs to. Among timed events
/// whose window (`start - 10 min` to `end`) holds `now`, one whose title
/// matches `meeting_title` wins; otherwise the one that started last.
pub fn match_event<'a>(
    events: &'a [CalendarEventOut],
    now: DateTime<Utc>,
    meeting_title: &str,
) -> Option<&'a CalendarEventOut> {
    let overlapping: Vec<&CalendarEventOut> = events
        .iter()
        .filter(|event| !event.all_day)
        .filter(|event| {
            let end = event
                .end
                .unwrap_or(event.start + Duration::minutes(ASSUMED_LENGTH_MINUTES));
            event.start - Duration::minutes(EARLY_START_MINUTES) <= now && now <= end
        })
        .collect();
    overlapping
        .iter()
        .copied()
        .filter(|event| same_title(&event.title, meeting_title))
        .max_by_key(|event| event.start)
        .or_else(|| overlapping.iter().copied().max_by_key(|event| event.start))
}

/// The name to send for an attendee. A new person needs one, so it falls back
/// to the address's local part. Someone already known keeps their name unless
/// it is only that fallback and the invite now has a real one: the calendar
/// is the weakest source and shouldn't rename people Linear or Notion named.
async fn name_for(pool: &SqlitePool, attendee: &CalendarAttendee) -> Result<String, String> {
    let local_part = attendee
        .email
        .split('@')
        .next()
        .unwrap_or(&attendee.email)
        .to_string();
    let known: Option<String> = sqlx::query_scalar(
        "SELECT p.name FROM people p WHERE p.email = ?
         UNION ALL
         SELECT p.name FROM people p JOIN person_sources s ON s.person_id = p.id
         WHERE s.source = ? AND s.external_id = ?
         LIMIT 1",
    )
    .bind(&attendee.email)
    .bind(SOURCE_CALENDAR)
    .bind(&attendee.email)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(match (known, &attendee.name) {
        (None, Some(name)) => name.clone(),
        (None, None) => local_part,
        (Some(current), Some(name))
            if current.eq_ignore_ascii_case(&local_part)
                || current.eq_ignore_ascii_case(&attendee.email) =>
        {
            name.clone()
        }
        // Empty leaves the stored name alone (see `upsert_from_source`).
        (Some(_), _) => String::new(),
    })
}

/// Adds the guests of the invite overlapping `now` as the meeting's attendees.
/// Returns how many were linked. Does nothing when the meeting already has
/// calendar attendees (a resumed recording binds again) or no invite matches.
/// An attendee whose address is `my_email` is marked as the user when nobody
/// is yet.
pub async fn link_calendar_attendees(
    pool: &SqlitePool,
    meeting_id: &str,
    now: DateTime<Utc>,
    snapshot: &CalendarSnapshot,
    my_email: Option<&str>,
) -> Result<usize, String> {
    let already: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM meeting_attendees WHERE meeting_id = ? AND source = ?",
    )
    .bind(meeting_id)
    .bind(SOURCE_CALENDAR)
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;
    if already > 0 {
        return Ok(0);
    }
    let title: Option<String> = sqlx::query_scalar("SELECT title FROM meetings WHERE id = ?")
        .bind(meeting_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;
    let Some(title) = title else {
        return Err(format!("Meeting {meeting_id} does not exist"));
    };
    let Some(event) = match_event(&snapshot.events, now, &title) else {
        return Ok(0);
    };
    let my_email = my_email
        .map(|email| email.trim().to_lowercase())
        .filter(|email| !email.is_empty());

    let mut linked = 0;
    for attendee in &event.attendees {
        let result: Result<(), String> = async {
            let input = PersonInput {
                source: SOURCE_CALENDAR.to_string(),
                external_id: attendee.email.clone(),
                name: name_for(pool, attendee).await?,
                email: Some(attendee.email.clone()),
                ..Default::default()
            };
            let person_id = super::upsert_from_source(pool, &input).await?;
            super::add_meeting_attendee(pool, meeting_id, &person_id, SOURCE_CALENDAR).await?;
            if my_email.as_deref() == Some(attendee.email.as_str())
                && super::me(pool).await?.is_none()
            {
                super::update_person(
                    pool,
                    &person_id,
                    &PersonEdit {
                        is_me: Some(true),
                        ..Default::default()
                    },
                )
                .await?;
            }
            Ok(())
        }
        .await;
        match result {
            Ok(()) => linked += 1,
            Err(error) => {
                log::warn!("Calendar attendee for meeting {meeting_id} was not linked: {error}")
            }
        }
    }
    Ok(linked)
}

/// Links attendees for a recording's freshly bound meeting in the background.
/// Never fails the recording: problems are logged.
pub fn spawn_for_recording<R: Runtime>(app: &AppHandle<R>, meeting_id: String) {
    let Some((snapshot, my_email)) = crate::calendar::linking_inputs(app) else {
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<crate::state::AppState>() else {
            return;
        };
        let pool = state.db_manager.pool();
        match link_calendar_attendees(
            pool,
            &meeting_id,
            Utc::now(),
            &snapshot,
            my_email.as_deref(),
        )
        .await
        {
            Ok(0) => {}
            Ok(count) => {
                log::info!("Linked {count} calendar attendees to meeting {meeting_id}");
                let _ = app.emit(
                    "meeting-attendees-updated",
                    serde_json::json!({ "meeting_id": meeting_id }),
                );
            }
            Err(error) => {
                log::warn!("Could not link calendar attendees to meeting {meeting_id}: {error}")
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::people::{
        get_person, list_people, meeting_attendees, upsert_from_source, PersonInput,
    };
    use chrono::TimeZone;

    async fn pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn meeting(pool: &SqlitePool, title: &str) -> String {
        crate::database::repositories::meeting::MeetingsRepository::create_meeting(
            pool, title, None, false,
        )
        .await
        .unwrap()
    }

    fn at(hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 26, hour, minute, 0).unwrap()
    }

    fn guest(name: Option<&str>, email: &str, organizer: bool) -> CalendarAttendee {
        CalendarAttendee {
            name: name.map(str::to_string),
            email: email.to_string(),
            organizer,
        }
    }

    fn event(title: &str, start: DateTime<Utc>, minutes: i64) -> CalendarEventOut {
        CalendarEventOut {
            uid: format!("{title}@example.com"),
            title: title.to_string(),
            start,
            end: Some(start + Duration::minutes(minutes)),
            location: None,
            all_day: false,
            attendees: Vec::new(),
        }
    }

    fn snapshot(events: Vec<CalendarEventOut>) -> CalendarSnapshot {
        CalendarSnapshot {
            fetched_at: at(9, 0),
            events,
        }
    }

    #[test]
    fn matches_events_overlapping_now_including_ten_minutes_early() {
        let events = vec![event("Standup", at(10, 0), 30)];
        assert!(match_event(&events, at(9, 50), "").is_some());
        assert!(match_event(&events, at(10, 30), "").is_some());
        assert!(match_event(&events, at(9, 49), "").is_none());
        assert!(match_event(&events, at(10, 31), "").is_none());
    }

    #[test]
    fn prefers_a_matching_title_then_the_latest_start() {
        let events = vec![
            event("Planning", at(9, 0), 120),
            event("Standup", at(10, 0), 30),
            event("Lunch", at(9, 30), 90),
        ];
        assert_eq!(
            match_event(&events, at(10, 5), " planning ").unwrap().title,
            "Planning"
        );
        assert_eq!(
            match_event(&events, at(10, 5), "Meeting 26_11_26_10_05_00")
                .unwrap()
                .title,
            "Standup"
        );
    }

    #[test]
    fn ignores_all_day_events_and_finds_nothing_outside_any_window() {
        let mut holiday = event("Holiday", at(0, 0), 24 * 60);
        holiday.all_day = true;
        let events = vec![holiday, event("Standup", at(10, 0), 30)];
        assert!(match_event(&events, at(15, 0), "Holiday").is_none());
        assert!(match_event(&[], at(15, 0), "").is_none());
    }

    #[tokio::test]
    async fn links_the_invites_guests_as_attendees() {
        let pool = pool().await;
        let meeting_id = meeting(&pool, "Planning").await;
        let mut planning = event("Planning", at(10, 0), 60);
        planning.attendees = vec![
            guest(Some("Priya Shah"), "priya@acme.com", true),
            guest(None, "dev.patel@acme.com", false),
        ];
        let linked = link_calendar_attendees(
            &pool,
            &meeting_id,
            at(10, 2),
            &snapshot(vec![planning]),
            None,
        )
        .await
        .unwrap();
        assert_eq!(linked, 2);
        let attendees = meeting_attendees(&pool, &meeting_id).await.unwrap();
        let names: Vec<&str> = attendees.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["dev.patel", "Priya Shah"]);
        assert!(attendees
            .iter()
            .all(|p| p.sources.iter().any(|s| s.source == SOURCE_CALENDAR)));
        assert!(attendees.iter().all(|p| !p.is_me));
        let source: String = sqlx::query_scalar("SELECT source FROM meeting_attendees LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(source, SOURCE_CALENDAR);
    }

    #[tokio::test]
    async fn links_nothing_without_a_matching_event_or_twice() {
        let pool = pool().await;
        let meeting_id = meeting(&pool, "Planning").await;
        let mut planning = event("Planning", at(10, 0), 60);
        planning.attendees = vec![guest(Some("Priya Shah"), "priya@acme.com", true)];
        let feed = snapshot(vec![planning]);
        assert_eq!(
            link_calendar_attendees(&pool, &meeting_id, at(14, 0), &feed, None)
                .await
                .unwrap(),
            0
        );
        assert!(list_people(&pool).await.unwrap().is_empty());
        assert_eq!(
            link_calendar_attendees(&pool, &meeting_id, at(10, 0), &feed, None)
                .await
                .unwrap(),
            1
        );
        // A resumed recording binds the same meeting again.
        assert_eq!(
            link_calendar_attendees(&pool, &meeting_id, at(10, 0), &feed, None)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn keeps_names_other_sources_gave() {
        let pool = pool().await;
        let id = upsert_from_source(
            &pool,
            &PersonInput {
                source: crate::people::SOURCE_LINEAR.into(),
                external_id: "u1".into(),
                name: "Dev Patel".into(),
                email: Some("dev@acme.com".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let meeting_id = meeting(&pool, "Sync").await;
        let mut sync = event("Sync", at(10, 0), 30);
        sync.attendees = vec![guest(Some("dev"), "dev@acme.com", false)];
        link_calendar_attendees(&pool, &meeting_id, at(10, 0), &snapshot(vec![sync]), None)
            .await
            .unwrap();
        let person = get_person(&pool, &id).await.unwrap().unwrap();
        assert_eq!(person.name, "Dev Patel");
        assert_eq!(person.sources.len(), 2);
        assert_eq!(list_people(&pool).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_later_invite_names_someone_known_only_by_address() {
        let pool = pool().await;
        let first = meeting(&pool, "One").await;
        let mut one = event("One", at(9, 0), 30);
        one.attendees = vec![guest(None, "ana@acme.com", false)];
        link_calendar_attendees(&pool, &first, at(9, 0), &snapshot(vec![one]), None)
            .await
            .unwrap();
        let second = meeting(&pool, "Two").await;
        let mut two = event("Two", at(11, 0), 30);
        two.attendees = vec![guest(Some("Ana Ruiz"), "ana@acme.com", false)];
        link_calendar_attendees(&pool, &second, at(11, 0), &snapshot(vec![two]), None)
            .await
            .unwrap();
        let people = list_people(&pool).await.unwrap();
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].name, "Ana Ruiz");
    }

    #[tokio::test]
    async fn my_email_marks_me_only_when_nobody_is() {
        let pool = pool().await;
        let meeting_id = meeting(&pool, "Planning").await;
        let mut planning = event("Planning", at(10, 0), 60);
        planning.attendees = vec![
            guest(Some("Priya Shah"), "priya@acme.com", true),
            guest(Some("Sam Lee"), "sam@acme.com", false),
        ];
        let feed = snapshot(vec![planning]);
        link_calendar_attendees(&pool, &meeting_id, at(10, 0), &feed, Some(" Sam@Acme.com "))
            .await
            .unwrap();
        let me = crate::people::me(&pool).await.unwrap().unwrap();
        assert_eq!(me.email.as_deref(), Some("sam@acme.com"));

        // Someone else is already the user: a different "my email" changes nothing.
        let other = meeting(&pool, "Planning").await;
        link_calendar_attendees(&pool, &other, at(10, 0), &feed, Some("priya@acme.com"))
            .await
            .unwrap();
        let me = crate::people::me(&pool).await.unwrap().unwrap();
        assert_eq!(me.email.as_deref(), Some("sam@acme.com"));
        let marked: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM people WHERE is_me = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(marked, 1);
    }
}
