//! A local directory of people (`people`, `person_sources`) and who attended
//! each meeting (`meeting_attendees`).
//!
//! People come from Linear, Notion, calendar invites, or the user. Sources
//! call [`upsert_from_source`]; the user's own edits go through
//! [`update_person`] and lock the fields they touched, so a later sync never
//! overwrites them. Nothing here leaves the Mac.
//!
//! Consumers read a meeting's [`MeetingContext`]: its attendees, the user, and
//! a vocabulary of names and project terms for transcription. See
//! `docs/superpowers/specs/2026-11-26-attendee-context-design.md`.

pub mod calendar_link;
pub mod commands;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

/// Where a person (or one of their projects) came from.
pub const SOURCE_LINEAR: &str = "linear";
pub const SOURCE_NOTION: &str = "notion";
pub const SOURCE_CALENDAR: &str = "calendar";
pub const SOURCE_MANUAL: &str = "manual";

/// Fields a manual edit locks against later syncs.
const LOCKABLE: [&str; 6] = ["name", "email", "role", "team", "aliases", "notes"];

/// Longest vocabulary handed to transcription.
pub const MAX_VOCABULARY: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// `linear`, `notion` or `manual`.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonSourceRef {
    pub source: String,
    pub external_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub id: String,
    pub name: String,
    pub email: Option<String>,
    pub role: Option<String>,
    pub team: Option<String>,
    pub aliases: Vec<String>,
    pub projects: Vec<Project>,
    pub notes: Option<String>,
    pub is_me: bool,
    pub locked_fields: Vec<String>,
    pub sources: Vec<PersonSourceRef>,
}

/// What a source knows about one person. Empty fields leave the stored value
/// alone; `projects` replaces only this source's projects.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonInput {
    pub source: String,
    pub external_id: String,
    pub name: String,
    pub email: Option<String>,
    pub role: Option<String>,
    pub team: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub projects: Vec<Project>,
}

/// A user's edit. `None` leaves a field alone; `Some("")` clears it. Every
/// field set here is locked against syncs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonEdit {
    pub name: Option<String>,
    pub email: Option<String>,
    pub role: Option<String>,
    pub team: Option<String>,
    pub aliases: Option<Vec<String>>,
    pub notes: Option<String>,
    pub is_me: Option<bool>,
}

/// Everything consumers need about a meeting's people.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingContext {
    pub attendees: Vec<Person>,
    /// The user, when one person is marked `is_me`.
    pub me: Option<Person>,
    /// Names, aliases, teams and project titles of the attendees (and the
    /// user), deduplicated, for transcription vocabulary boosting.
    pub vocabulary: Vec<String>,
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn clean(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn normalize_email(value: Option<&str>) -> Option<String> {
    clean(value).map(|v| v.to_lowercase())
}

fn json_vec<T: for<'de> Deserialize<'de>>(raw: &str) -> Vec<T> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn to_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[]".to_string())
}

fn dedup_strings(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    values
        .into_iter()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && seen.insert(v.to_lowercase()))
        .collect()
}

async fn sources_of(pool: &SqlitePool, person_id: &str) -> Result<Vec<PersonSourceRef>, String> {
    let rows = sqlx::query(
        "SELECT source, external_id FROM person_sources WHERE person_id = ? ORDER BY source, external_id",
    )
    .bind(person_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|row| PersonSourceRef {
            source: row.get("source"),
            external_id: row.get("external_id"),
        })
        .collect())
}

fn person_from_row(row: &sqlx::sqlite::SqliteRow) -> Person {
    Person {
        id: row.get("id"),
        name: row.get("name"),
        email: row.get("email"),
        role: row.get("role"),
        team: row.get("team"),
        aliases: json_vec(&row.get::<String, _>("aliases")),
        projects: json_vec(&row.get::<String, _>("projects")),
        notes: row.get("notes"),
        is_me: row.get::<i64, _>("is_me") != 0,
        locked_fields: json_vec(&row.get::<String, _>("locked_fields")),
        sources: Vec::new(),
    }
}

const PERSON_COLUMNS: &str =
    "id, name, email, role, team, aliases, projects, notes, is_me, locked_fields";

pub async fn get_person(pool: &SqlitePool, id: &str) -> Result<Option<Person>, String> {
    let row = sqlx::query(&format!("SELECT {PERSON_COLUMNS} FROM people WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;
    match row {
        Some(row) => {
            let mut person = person_from_row(&row);
            person.sources = sources_of(pool, &person.id).await?;
            Ok(Some(person))
        }
        None => Ok(None),
    }
}

/// Everyone, sorted by name.
pub async fn list_people(pool: &SqlitePool) -> Result<Vec<Person>, String> {
    let rows = sqlx::query(&format!(
        "SELECT {PERSON_COLUMNS} FROM people ORDER BY name COLLATE NOCASE"
    ))
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;
    let mut people: Vec<Person> = rows.iter().map(person_from_row).collect();
    for person in &mut people {
        person.sources = sources_of(pool, &person.id).await?;
    }
    Ok(people)
}

async fn find_for_input(pool: &SqlitePool, input: &PersonInput) -> Result<Option<String>, String> {
    if let Some(id) = sqlx::query_scalar::<_, String>(
        "SELECT person_id FROM person_sources WHERE source = ? AND external_id = ?",
    )
    .bind(&input.source)
    .bind(&input.external_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?
    {
        return Ok(Some(id));
    }
    if let Some(email) = normalize_email(input.email.as_deref()) {
        return sqlx::query_scalar::<_, String>("SELECT id FROM people WHERE email = ?")
            .bind(email)
            .fetch_optional(pool)
            .await
            .map_err(|e| e.to_string());
    }
    Ok(None)
}

/// Adds or updates a person from a source. Matches by (source, external id),
/// then by email; otherwise creates a person. Fields the user locked are left
/// alone, empty incoming fields never clear stored ones, aliases are unioned,
/// and only this source's projects are replaced. Returns the person's id.
pub async fn upsert_from_source(pool: &SqlitePool, input: &PersonInput) -> Result<String, String> {
    if input.source.trim().is_empty() || input.external_id.trim().is_empty() {
        return Err("A person from a source needs a source and an external id".to_string());
    }
    let name = input.name.trim();
    let email = normalize_email(input.email.as_deref());
    let stamp = now();

    let person_id = match find_for_input(pool, input).await? {
        Some(id) => {
            let existing = get_person(pool, &id)
                .await?
                .ok_or_else(|| "Person disappeared during sync".to_string())?;
            let locked = |field: &str| existing.locked_fields.iter().any(|f| f == field);
            let pick = |field: &str, incoming: Option<String>, current: Option<String>| {
                if locked(field) {
                    current
                } else {
                    incoming.or(current)
                }
            };
            let name = if locked("name") || name.is_empty() {
                existing.name.clone()
            } else {
                name.to_string()
            };
            let email = pick("email", email.clone(), existing.email.clone());
            let role = pick("role", clean(input.role.as_deref()), existing.role.clone());
            let team = pick("team", clean(input.team.as_deref()), existing.team.clone());
            let aliases = if locked("aliases") {
                existing.aliases.clone()
            } else {
                dedup_strings(
                    existing
                        .aliases
                        .iter()
                        .cloned()
                        .chain(input.aliases.iter().cloned()),
                )
            };
            let mut projects: Vec<Project> = existing
                .projects
                .iter()
                .filter(|p| p.source != input.source)
                .cloned()
                .collect();
            projects.extend(input.projects.iter().cloned());
            // Another person already holding this email keeps it.
            let email = match &email {
                Some(e) if existing.email.as_deref() != Some(e.as_str()) => {
                    let taken: Option<String> =
                        sqlx::query_scalar("SELECT id FROM people WHERE email = ? AND id != ?")
                            .bind(e)
                            .bind(&id)
                            .fetch_optional(pool)
                            .await
                            .map_err(|e| e.to_string())?;
                    if taken.is_some() {
                        existing.email.clone()
                    } else {
                        email
                    }
                }
                _ => email,
            };
            sqlx::query(
                "UPDATE people SET name = ?, email = ?, role = ?, team = ?, aliases = ?, projects = ?, updated_at = ? WHERE id = ?",
            )
            .bind(&name)
            .bind(&email)
            .bind(&role)
            .bind(&team)
            .bind(to_json(&aliases))
            .bind(to_json(&projects))
            .bind(&stamp)
            .bind(&id)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
            id
        }
        None => {
            if name.is_empty() {
                return Err("A new person needs a name".to_string());
            }
            let id = uuid::Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO people (id, name, email, role, team, aliases, projects, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(name)
            .bind(&email)
            .bind(clean(input.role.as_deref()))
            .bind(clean(input.team.as_deref()))
            .bind(to_json(&dedup_strings(input.aliases.iter().cloned())))
            .bind(to_json(&input.projects))
            .bind(&stamp)
            .bind(&stamp)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
            id
        }
    };

    sqlx::query(
        "INSERT INTO person_sources (source, external_id, person_id, synced_at) VALUES (?, ?, ?, ?)
         ON CONFLICT(source, external_id) DO UPDATE SET person_id = excluded.person_id, synced_at = excluded.synced_at",
    )
    .bind(&input.source)
    .bind(&input.external_id)
    .bind(&person_id)
    .bind(&stamp)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(person_id)
}

/// Creates a person the user typed in.
pub async fn create_manual_person(pool: &SqlitePool, edit: &PersonEdit) -> Result<Person, String> {
    let name = clean(edit.name.as_deref()).ok_or_else(|| "A person needs a name".to_string())?;
    let id = uuid::Uuid::new_v4().to_string();
    let input = PersonInput {
        source: SOURCE_MANUAL.to_string(),
        external_id: id.clone(),
        name,
        email: edit.email.clone(),
        ..Default::default()
    };
    let person_id = upsert_from_source(pool, &input).await?;
    update_person(pool, &person_id, edit).await
}

/// Applies the user's edit and locks every field it sets. Marking someone as
/// the user clears the mark from everyone else.
pub async fn update_person(
    pool: &SqlitePool,
    id: &str,
    edit: &PersonEdit,
) -> Result<Person, String> {
    let existing = get_person(pool, id)
        .await?
        .ok_or_else(|| "That person no longer exists".to_string())?;
    let mut locked: Vec<String> = existing.locked_fields.clone();
    let mut lock = |field: &str| {
        if LOCKABLE.contains(&field) && !locked.iter().any(|f| f == field) {
            locked.push(field.to_string());
        }
    };

    let name = match &edit.name {
        Some(name) => {
            let name = name.trim();
            if name.is_empty() {
                return Err("A person needs a name".to_string());
            }
            lock("name");
            name.to_string()
        }
        None => existing.name.clone(),
    };
    let mut text =
        |field: &str, value: &Option<String>, current: Option<String>, email: bool| match value {
            Some(v) => {
                lock(field);
                if email {
                    normalize_email(Some(v))
                } else {
                    clean(Some(v))
                }
            }
            None => current,
        };
    let email = text("email", &edit.email, existing.email.clone(), true);
    let role = text("role", &edit.role, existing.role.clone(), false);
    let team = text("team", &edit.team, existing.team.clone(), false);
    let notes = text("notes", &edit.notes, existing.notes.clone(), false);
    let aliases = match &edit.aliases {
        Some(aliases) => {
            lock("aliases");
            dedup_strings(aliases.iter().cloned())
        }
        None => existing.aliases.clone(),
    };
    if let Some(e) = &email {
        let taken: Option<String> =
            sqlx::query_scalar("SELECT id FROM people WHERE email = ? AND id != ?")
                .bind(e)
                .bind(id)
                .fetch_optional(pool)
                .await
                .map_err(|e| e.to_string())?;
        if taken.is_some() {
            return Err(format!("Someone else already has the email {e}"));
        }
    }

    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    if edit.is_me == Some(true) {
        sqlx::query("UPDATE people SET is_me = 0 WHERE id != ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    let is_me = edit.is_me.unwrap_or(existing.is_me);
    sqlx::query(
        "UPDATE people SET name = ?, email = ?, role = ?, team = ?, aliases = ?, notes = ?, is_me = ?, locked_fields = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&name)
    .bind(&email)
    .bind(&role)
    .bind(&team)
    .bind(to_json(&aliases))
    .bind(&notes)
    .bind(is_me as i64)
    .bind(to_json(&locked))
    .bind(now())
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    get_person(pool, id)
        .await?
        .ok_or_else(|| "That person no longer exists".to_string())
}

/// Deletes a person, their source links and their meeting attendance.
pub async fn delete_person(pool: &SqlitePool, id: &str) -> Result<(), String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    for statement in [
        "DELETE FROM meeting_attendees WHERE person_id = ?",
        "DELETE FROM person_sources WHERE person_id = ?",
        "DELETE FROM people WHERE id = ?",
    ] {
        sqlx::query(statement)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    tx.commit().await.map_err(|e| e.to_string())
}

/// Removes every link to a source (e.g. when its integration is disconnected).
/// People left with no source and no manual edits are deleted.
pub async fn forget_source(pool: &SqlitePool, source: &str) -> Result<u64, String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query("DELETE FROM person_sources WHERE source = ?")
        .bind(source)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    let orphans: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM people WHERE locked_fields = '[]' AND is_me = 0
         AND id NOT IN (SELECT person_id FROM person_sources)
         AND id NOT IN (SELECT person_id FROM meeting_attendees)",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;
    for id in &orphans {
        sqlx::query("DELETE FROM people WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    // Projects from the source go too.
    let rows = sqlx::query("SELECT id, projects FROM people")
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    for row in rows {
        let id: String = row.get("id");
        let projects: Vec<Project> = json_vec(&row.get::<String, _>("projects"));
        let kept: Vec<Project> = projects
            .iter()
            .filter(|p| p.source != source)
            .cloned()
            .collect();
        if kept.len() != projects.len() {
            sqlx::query("UPDATE people SET projects = ? WHERE id = ?")
                .bind(to_json(&kept))
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(orphans.len() as u64)
}

pub async fn add_meeting_attendee(
    pool: &SqlitePool,
    meeting_id: &str,
    person_id: &str,
    source: &str,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO meeting_attendees (meeting_id, person_id, source, created_at) VALUES (?, ?, ?, ?)
         ON CONFLICT(meeting_id, person_id) DO NOTHING",
    )
    .bind(meeting_id)
    .bind(person_id)
    .bind(source)
    .bind(now())
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn remove_meeting_attendee(
    pool: &SqlitePool,
    meeting_id: &str,
    person_id: &str,
) -> Result<(), String> {
    sqlx::query("DELETE FROM meeting_attendees WHERE meeting_id = ? AND person_id = ?")
        .bind(meeting_id)
        .bind(person_id)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// A meeting's attendees, sorted by name.
pub async fn meeting_attendees(pool: &SqlitePool, meeting_id: &str) -> Result<Vec<Person>, String> {
    let rows = sqlx::query(&format!(
        "SELECT {} FROM people p JOIN meeting_attendees a ON a.person_id = p.id
         WHERE a.meeting_id = ? ORDER BY p.name COLLATE NOCASE",
        PERSON_COLUMNS
            .split(", ")
            .map(|c| format!("p.{c}"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
    .bind(meeting_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;
    let mut people: Vec<Person> = rows.iter().map(person_from_row).collect();
    for person in &mut people {
        person.sources = sources_of(pool, &person.id).await?;
    }
    Ok(people)
}

pub async fn me(pool: &SqlitePool) -> Result<Option<Person>, String> {
    let id: Option<String> = sqlx::query_scalar("SELECT id FROM people WHERE is_me = 1 LIMIT 1")
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;
    match id {
        Some(id) => get_person(pool, &id).await,
        None => Ok(None),
    }
}

/// Names, aliases, teams and project titles, deduplicated and capped.
pub fn vocabulary_for(people: &[Person]) -> Vec<String> {
    let mut terms = Vec::new();
    for person in people {
        terms.push(person.name.clone());
        terms.extend(person.aliases.iter().cloned());
        terms.extend(person.team.iter().cloned());
        terms.extend(person.projects.iter().map(|p| p.title.clone()));
    }
    let mut terms = dedup_strings(terms);
    terms.truncate(MAX_VOCABULARY);
    terms
}

/// The people context for a meeting: its attendees, the user and a
/// transcription vocabulary.
pub async fn meeting_context(
    pool: &SqlitePool,
    meeting_id: &str,
) -> Result<MeetingContext, String> {
    let attendees = meeting_attendees(pool, meeting_id).await?;
    let me = me(pool).await?;
    let mut everyone = attendees.clone();
    if let Some(me) = &me {
        if !everyone.iter().any(|p| p.id == me.id) {
            everyone.push(me.clone());
        }
    }
    let vocabulary = vocabulary_for(&everyone);
    Ok(MeetingContext {
        attendees,
        me,
        vocabulary,
    })
}

/// One line per attendee for an LLM prompt, e.g.
/// `- Priya Shah (Product manager, Growth). Projects: Offline mode, Pricing`.
/// Stops before `max_chars`. Empty when there are no attendees.
pub fn render_context_block(context: &MeetingContext, max_chars: usize) -> String {
    let mut out = String::new();
    for person in &context.attendees {
        let mut line = format!("- {}", person.name);
        let details: Vec<&str> = [person.role.as_deref(), person.team.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        if !details.is_empty() {
            line.push_str(&format!(" ({})", details.join(", ")));
        }
        if context.me.as_ref().map(|m| &m.id) == Some(&person.id) {
            line.push_str(" [the user]");
        }
        if !person.projects.is_empty() {
            let titles: Vec<&str> = person
                .projects
                .iter()
                .take(5)
                .map(|p| p.title.as_str())
                .collect();
            line.push_str(&format!(". Projects: {}", titles.join(", ")));
        }
        line.push('\n');
        if out.len() + line.len() > max_chars {
            break;
        }
        out.push_str(&line);
    }
    out
}

/// The attendee a short name refers to: a unique match on full name, first
/// name or alias (case-insensitive). `None` when nobody or several match.
pub fn resolve_name<'a>(context: &'a MeetingContext, short: &str) -> Option<&'a Person> {
    let needle = short.trim().to_lowercase();
    if needle.is_empty() {
        return None;
    }
    let matches: Vec<&Person> = context
        .attendees
        .iter()
        .filter(|p| {
            p.name.to_lowercase() == needle
                || p.name
                    .split_whitespace()
                    .next()
                    .map(str::to_lowercase)
                    .as_deref()
                    == Some(needle.as_str())
                || p.aliases.iter().any(|a| a.to_lowercase() == needle)
        })
        .collect();
    match matches.as_slice() {
        [one] => Some(one),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
