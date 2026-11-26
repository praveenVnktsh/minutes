//! People from a Notion database, through an internal integration token.
//!
//! The user shares a team-directory database with their integration and picks
//! it in settings. Requests send the token and these read-only calls, nothing
//! else: search for databases, read the chosen database's schema, query its
//! pages, and read the titles of related project pages (capped). Each page
//! becomes a [`PersonInput`] with source `notion`.
//!
//! Properties map to fields by type and name ([`detect_mapping`]), and the
//! user can override each one ([`NotionOverrides`]).

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::http::{HttpError, Method, Request, Transport};
use super::sync::{ConnectionTest, Fetched};
use super::{PersonInput, Project, SOURCE_NOTION};

pub const NOTION_API: &str = "https://api.notion.com/v1";
pub const NOTION_VERSION: &str = "2022-06-28";

/// Related pages whose titles are fetched per sync. Each is one request.
pub const MAX_RELATION_LOOKUPS: usize = 100;
/// Projects kept per person.
pub const MAX_PROJECTS_PER_PERSON: usize = 10;
/// A runaway cursor must not loop forever.
const MAX_PAGES: usize = 200;
/// Pause between page lookups, to stay under Notion's ~3 requests a second.
pub const LOOKUP_PACE: Duration = Duration::from_millis(350);

const EMAIL_NAMES: [&str; 4] = ["email", "e-mail", "work email", "email address"];
const ROLE_NAMES: [&str; 4] = ["role", "title", "position", "job title"];
const TEAM_NAMES: [&str; 4] = ["team", "department", "teams", "departments"];
const PROJECT_NAMES: [&str; 2] = ["projects", "project"];
const PROJECT_TYPES: [&str; 2] = ["relation", "multi_select"];

/// Which database property feeds each person field. `None`: not mapped.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotionMapping {
    pub name: Option<String>,
    pub email: Option<String>,
    pub role: Option<String>,
    pub team: Option<String>,
    pub projects: Option<String>,
}

/// The user's overrides. `None`: use the detected property; `Some("")`: don't
/// map this field; `Some(name)`: use that property.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotionOverrides {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub team: Option<String>,
    #[serde(default)]
    pub projects: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotionProperty {
    pub name: String,
    /// Notion's property type: `title`, `email`, `select`, `relation`, ...
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotionDatabase {
    pub id: String,
    pub title: String,
    pub url: Option<String>,
    pub properties: Vec<NotionProperty>,
    pub detected: NotionMapping,
}

fn find<'a>(
    properties: &'a [NotionProperty],
    names: &[&str],
    accept: impl Fn(&NotionProperty) -> bool,
) -> Option<&'a NotionProperty> {
    // Earlier names win: "Role" before "Title".
    names.iter().find_map(|wanted| {
        properties
            .iter()
            .find(|p| p.name.trim().eq_ignore_ascii_case(wanted) && accept(p))
    })
}

/// The default mapping for a database's properties:
/// - the title property → name
/// - an email-type property, or one named Email → email
/// - Role / Title / Position → role
/// - Team / Department → team
/// - a relation or multi-select named Projects → projects
pub fn detect_mapping(properties: &[NotionProperty]) -> NotionMapping {
    let name = properties.iter().find(|p| p.kind == "title");
    let email = properties
        .iter()
        .find(|p| p.kind == "email")
        .or_else(|| find(properties, &EMAIL_NAMES, |_| true));
    let role = find(properties, &ROLE_NAMES, |p| p.kind != "title");
    let team = find(properties, &TEAM_NAMES, |p| p.kind != "title");
    let projects = find(properties, &PROJECT_NAMES, |p| {
        PROJECT_TYPES.contains(&p.kind.as_str())
    });
    let named = |p: Option<&NotionProperty>| p.map(|p| p.name.clone());
    NotionMapping {
        name: named(name),
        email: named(email),
        role: named(role),
        team: named(team),
        projects: named(projects),
    }
}

/// The detected mapping with the user's overrides applied. An override naming
/// a property the database doesn't have is an error.
pub fn resolve_mapping(
    properties: &[NotionProperty],
    overrides: &NotionOverrides,
) -> Result<NotionMapping, String> {
    let detected = detect_mapping(properties);
    let pick = |field: &str, over: &Option<String>, auto: Option<String>| match over {
        None => Ok(auto),
        Some(name) if name.trim().is_empty() => Ok(None),
        Some(name) => properties
            .iter()
            .find(|p| p.name == *name)
            .map(|p| Some(p.name.clone()))
            .ok_or_else(|| {
                format!("The Notion database has no property \"{name}\" (mapped to {field})")
            }),
    };
    let mapping = NotionMapping {
        name: pick("name", &overrides.name, detected.name)?,
        email: pick("email", &overrides.email, detected.email)?,
        role: pick("role", &overrides.role, detected.role)?,
        team: pick("team", &overrides.team, detected.team)?,
        projects: pick("projects", &overrides.projects, detected.projects)?,
    };
    if mapping.name.is_none() {
        return Err("Choose which Notion property holds each person's name".to_string());
    }
    Ok(mapping)
}

/// A user-facing message for a failed Notion request. Never includes the token.
pub fn describe_error(error: &HttpError) -> String {
    match error.status {
        None => format!("Could not reach Notion: {}", error.message),
        Some(401) => "Notion rejected the integration token".to_string(),
        Some(403) => format!(
            "The Notion integration is missing a capability: {}",
            error.message
        ),
        Some(404) => "Notion can't find that database. Share it with your integration \
                      (••• → Connections) and try again"
            .to_string(),
        Some(429) => "Notion is rate limiting requests; try again later".to_string(),
        Some(status) => format!("Notion returned an error ({status}): {}", error.message),
    }
}

/// Plain text of a rich-text array.
fn plain_text(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("plain_text").and_then(Value::as_str))
                .collect::<String>()
        })
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn title_of_database(db: &Value) -> String {
    let title = plain_text(db.get("title"));
    if title.is_empty() {
        "Untitled".to_string()
    } else {
        title
    }
}

fn properties_of(db: &Value) -> Vec<NotionProperty> {
    let mut properties: Vec<NotionProperty> = db
        .get("properties")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(key, prop)| NotionProperty {
                    name: prop
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(key)
                        .to_string(),
                    kind: prop
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    properties.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    properties
}

fn database_from(db: &Value) -> Option<NotionDatabase> {
    let id = db.get("id")?.as_str()?.to_string();
    let properties = properties_of(db);
    Some(NotionDatabase {
        id,
        title: title_of_database(db),
        url: db.get("url").and_then(Value::as_str).map(str::to_string),
        detected: detect_mapping(&properties),
        properties,
    })
}

/// The title of a page (its `title`-type property).
fn page_title(page: &Value) -> Option<String> {
    page.get("properties")
        .and_then(Value::as_object)?
        .values()
        .find(|p| p.get("type").and_then(Value::as_str) == Some("title"))
        .map(|p| plain_text(p.get("title")))
        .filter(|t| !t.is_empty())
}

/// Titles (and URLs) of related pages, by page id.
pub type RelatedPages = HashMap<String, (String, Option<String>)>;

/// The text values of a page property. Relations resolve through `related`
/// and are dropped when unresolved.
fn property_values(prop: &Value, related: &RelatedPages) -> Vec<(String, Option<String>)> {
    let kind = prop.get("type").and_then(Value::as_str).unwrap_or("");
    let value = prop.get(kind);
    let one = |text: String| {
        let text = text.trim().to_string();
        if text.is_empty() {
            vec![]
        } else {
            vec![(text, None)]
        }
    };
    let as_string = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or("").to_string();
    match kind {
        "title" | "rich_text" => one(plain_text(value)),
        "email" | "phone_number" | "url" => one(as_string(value)),
        "select" | "status" => one(as_string(value.and_then(|v| v.get("name")))),
        "multi_select" => value
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .flat_map(|i| one(as_string(i.get("name"))))
                    .collect()
            })
            .unwrap_or_default(),
        "people" => value
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .flat_map(|i| one(as_string(i.get("name"))))
                    .collect()
            })
            .unwrap_or_default(),
        "number" => value
            .and_then(Value::as_f64)
            .map(|n| one(n.to_string()))
            .unwrap_or_default(),
        "formula" => {
            let inner_kind = value.and_then(|v| v.get("type")).and_then(Value::as_str);
            match inner_kind {
                Some("string") => one(as_string(value.and_then(|v| v.get("string")))),
                Some("number") => value
                    .and_then(|v| v.get("number"))
                    .and_then(Value::as_f64)
                    .map(|n| one(n.to_string()))
                    .unwrap_or_default(),
                _ => vec![],
            }
        }
        "rollup" => value
            .and_then(|v| v.get("array"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .flat_map(|i| property_values(i, related))
                    .collect()
            })
            .unwrap_or_default(),
        "relation" => relation_ids(prop)
            .into_iter()
            .filter_map(|id| related.get(&id).cloned())
            .collect(),
        _ => vec![],
    }
}

fn relation_ids(prop: &Value) -> Vec<String> {
    prop.get("relation")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.get("id").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn joined(values: Vec<(String, Option<String>)>) -> Option<String> {
    let texts: Vec<String> = values.into_iter().map(|(t, _)| t).collect();
    (!texts.is_empty()).then(|| texts.join(", "))
}

/// Maps one database page to a person. `None` when the page has no name.
pub fn page_to_input(
    page: &Value,
    mapping: &NotionMapping,
    related: &RelatedPages,
) -> Option<PersonInput> {
    let id = page.get("id")?.as_str()?.to_string();
    let props = page.get("properties")?.as_object()?;
    let values = |field: &Option<String>| {
        field
            .as_ref()
            .and_then(|name| props.get(name))
            .map(|prop| property_values(prop, related))
            .unwrap_or_default()
    };
    let name = joined(values(&mapping.name))?;
    let email = values(&mapping.email).into_iter().next().map(|(t, _)| t);
    let mut seen = std::collections::HashSet::new();
    let projects = values(&mapping.projects)
        .into_iter()
        .filter(|(title, _)| seen.insert(title.to_lowercase()))
        .take(MAX_PROJECTS_PER_PERSON)
        .map(|(title, url)| Project {
            title,
            url,
            source: SOURCE_NOTION.to_string(),
        })
        .collect();
    Some(PersonInput {
        source: SOURCE_NOTION.to_string(),
        external_id: id,
        name,
        email,
        role: joined(values(&mapping.role)),
        team: joined(values(&mapping.team)),
        aliases: Vec::new(),
        projects,
    })
}

pub struct NotionClient<'a, T: Transport> {
    transport: &'a T,
    token: &'a str,
    pace: Duration,
}

impl<'a, T: Transport> NotionClient<'a, T> {
    pub fn new(transport: &'a T, token: &'a str) -> Self {
        Self {
            transport,
            token,
            pace: LOOKUP_PACE,
        }
    }

    /// Changes the pause between page lookups (tests use zero).
    pub fn with_pace(mut self, pace: Duration) -> Self {
        self.pace = pace;
        self
    }

    fn request(&self, method: Method, path: &str, body: Option<Value>) -> Request {
        Request {
            method,
            url: format!("{NOTION_API}{path}"),
            headers: vec![
                (
                    "Authorization".to_string(),
                    format!("Bearer {}", self.token.trim()),
                ),
                ("Notion-Version".to_string(), NOTION_VERSION.to_string()),
                ("Content-Type".to_string(), "application/json".to_string()),
            ],
            body,
        }
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, HttpError> {
        self.transport.send(self.request(method, path, body)).await
    }

    /// Every result of a paginated POST endpoint.
    async fn paginate(&self, path: &str, body: Value) -> Result<Vec<Value>, String> {
        let mut results = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut body = body.clone();
            body["page_size"] = json!(100);
            if let Some(c) = &cursor {
                body["start_cursor"] = json!(c);
            }
            let page = self
                .send(Method::Post, path, Some(body))
                .await
                .map_err(|e| describe_error(&e))?;
            let items = page
                .get("results")
                .and_then(Value::as_array)
                .ok_or_else(|| "Unexpected Notion response: no results".to_string())?;
            results.extend(items.iter().cloned());
            let more = page
                .get("has_more")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            match (more, page.get("next_cursor").and_then(Value::as_str)) {
                (true, Some(next)) => cursor = Some(next.to_string()),
                _ => return Ok(results),
            }
        }
        Ok(results)
    }

    /// Databases the integration can see, sorted by title.
    pub async fn list_databases(&self) -> Result<Vec<NotionDatabase>, String> {
        let results = self
            .paginate(
                "/search",
                json!({ "filter": { "property": "object", "value": "database" } }),
            )
            .await?;
        let mut databases: Vec<NotionDatabase> = results
            .iter()
            .filter(|r| r.get("object").and_then(Value::as_str) == Some("database"))
            .filter_map(database_from)
            .collect();
        databases.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        Ok(databases)
    }

    pub async fn database(&self, id: &str) -> Result<NotionDatabase, String> {
        let db = self
            .send(Method::Get, &format!("/databases/{}", encode_id(id)), None)
            .await
            .map_err(|e| describe_error(&e))?;
        database_from(&db).ok_or_else(|| "Unexpected Notion response for the database".to_string())
    }

    async fn pages(&self, database_id: &str) -> Result<Vec<Value>, String> {
        self.paginate(
            &format!("/databases/{}/query", encode_id(database_id)),
            json!({}),
        )
        .await
    }

    /// Titles of related pages, at most [`MAX_RELATION_LOOKUPS`]. A failed or
    /// rate-limited lookup stops further lookups but never fails the sync; its
    /// reason comes back as a warning.
    async fn related_pages(&self, ids: Vec<String>) -> (RelatedPages, Option<String>) {
        let mut related = RelatedPages::new();
        for (index, id) in ids.into_iter().take(MAX_RELATION_LOOKUPS).enumerate() {
            if index > 0 && !self.pace.is_zero() {
                tokio::time::sleep(self.pace).await;
            }
            match self
                .send(Method::Get, &format!("/pages/{}", encode_id(&id)), None)
                .await
            {
                Ok(page) => {
                    if let Some(title) = page_title(&page) {
                        let url = page.get("url").and_then(Value::as_str).map(str::to_string);
                        related.insert(id, (title, url));
                    }
                }
                Err(error) => {
                    let reason = describe_error(&error);
                    log::warn!("Stopped resolving Notion related pages: {reason}");
                    return (
                        related,
                        Some(format!(
                            "Some related page titles couldn't be read: {reason}"
                        )),
                    );
                }
            }
        }
        (related, None)
    }

    /// Every named page of the database as a [`PersonInput`].
    pub async fn fetch_people(
        &self,
        database_id: &str,
        overrides: &NotionOverrides,
    ) -> Result<Fetched, String> {
        let database = self.database(database_id).await?;
        let mapping = resolve_mapping(&database.properties, overrides)?;
        let pages = self.pages(database_id).await?;

        // Related page ids the mapped properties point at, first seen first.
        let mut wanted = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for page in &pages {
            for field in [
                &mapping.name,
                &mapping.role,
                &mapping.team,
                &mapping.projects,
            ] {
                let Some(prop) = field
                    .as_ref()
                    .and_then(|name| page.get("properties").and_then(|p| p.get(name)))
                else {
                    continue;
                };
                for id in relation_ids(prop) {
                    if seen.insert(id.clone()) {
                        wanted.push(id);
                    }
                }
            }
        }
        let (related, warning) = if wanted.is_empty() {
            (RelatedPages::new(), None)
        } else {
            self.related_pages(wanted).await
        };

        Ok(Fetched {
            people: pages
                .iter()
                .filter(|p| p.get("archived").and_then(Value::as_bool) != Some(true))
                .filter_map(|p| page_to_input(p, &mapping, &related))
                .collect(),
            warnings: warning.into_iter().collect(),
        })
    }

    /// The workspace name, and the number of rows in the chosen database.
    pub async fn test_connection(
        &self,
        database_id: Option<&str>,
    ) -> Result<ConnectionTest, String> {
        let me = self
            .send(Method::Get, "/users/me", None)
            .await
            .map_err(|e| describe_error(&e))?;
        let workspace = me
            .pointer("/bot/workspace_name")
            .and_then(Value::as_str)
            .map(str::to_string);
        let user_count = match database_id {
            Some(id) => Some(self.pages(id).await?.len()),
            None => None,
        };
        Ok(ConnectionTest {
            workspace,
            user_count,
        })
    }
}

/// A Notion id is hex with optional dashes; anything else is refused so it
/// can't change the request path.
fn encode_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_hexdigit() || *c == '-')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::people::http::fake::FakeTransport;

    fn prop(name: &str, kind: &str) -> NotionProperty {
        NotionProperty {
            name: name.into(),
            kind: kind.into(),
        }
    }

    fn rich(text: &str) -> Value {
        json!([{ "type": "text", "plain_text": text }])
    }

    fn database_json() -> Value {
        json!({
            "object": "database",
            "id": "db-1",
            "url": "https://www.notion.so/db1",
            "title": rich("Team directory"),
            "properties": {
                "Name": { "id": "title", "name": "Name", "type": "title", "title": {} },
                "Work email": { "id": "a", "name": "Work email", "type": "email", "email": {} },
                "Role": { "id": "b", "name": "Role", "type": "rich_text", "rich_text": {} },
                "Department": { "id": "c", "name": "Department", "type": "select", "select": {} },
                "Projects": { "id": "d", "name": "Projects", "type": "relation", "relation": {} },
                "Tags": { "id": "e", "name": "Tags", "type": "multi_select", "multi_select": {} }
            }
        })
    }

    fn page(id: &str, name: &str, projects: &[&str]) -> Value {
        json!({
            "object": "page",
            "id": id,
            "properties": {
                "Name": { "type": "title", "title": rich(name) },
                "Work email": { "type": "email", "email": format!("{}@example.com", id) },
                "Role": { "type": "rich_text", "rich_text": rich("Engineer") },
                "Department": { "type": "select", "select": { "name": "Platform" } },
                "Projects": { "type": "relation", "relation":
                    projects.iter().map(|p| json!({ "id": p })).collect::<Vec<_>>(), "has_more": false },
                "Tags": { "type": "multi_select", "multi_select": [{ "name": "Mobile" }, { "name": "Web" }] }
            }
        })
    }

    fn query_page(results: Vec<Value>, next: Option<&str>) -> Value {
        json!({ "object": "list", "results": results, "has_more": next.is_some(), "next_cursor": next })
    }

    fn project_page(id: &str, title: &str) -> Value {
        json!({ "object": "page", "id": id, "url": format!("https://www.notion.so/{id}"),
                "properties": { "Project name": { "type": "title", "title": rich(title) } } })
    }

    #[test]
    fn detects_properties_by_type_then_name() {
        let props = vec![
            prop("Full name", "title"),
            prop("Contact", "email"),
            prop("Email", "rich_text"),
            prop("Title", "rich_text"),
            prop("Role", "select"),
            prop("Department", "select"),
            prop("Projects", "relation"),
        ];
        assert_eq!(
            detect_mapping(&props),
            NotionMapping {
                name: Some("Full name".into()),
                email: Some("Contact".into()),
                role: Some("Role".into()),
                team: Some("Department".into()),
                projects: Some("Projects".into()),
            }
        );

        // A property named Email when none has the email type; Title as role;
        // a rich-text "Projects" is not a project list.
        let props = vec![
            prop("Name", "title"),
            prop("email", "rich_text"),
            prop("Title", "rich_text"),
            prop("Team", "multi_select"),
            prop("Projects", "rich_text"),
        ];
        let mapping = detect_mapping(&props);
        assert_eq!(mapping.email.as_deref(), Some("email"));
        assert_eq!(mapping.role.as_deref(), Some("Title"));
        assert_eq!(mapping.team.as_deref(), Some("Team"));
        assert_eq!(mapping.projects, None);

        // The title property itself is never the role.
        let mapping = detect_mapping(&[prop("Title", "title")]);
        assert_eq!(mapping.name.as_deref(), Some("Title"));
        assert_eq!(mapping.role, None);
    }

    #[test]
    fn overrides_replace_clear_or_reject() {
        let props = vec![
            prop("Name", "title"),
            prop("Role", "rich_text"),
            prop("Squad", "select"),
            prop("Projects", "multi_select"),
        ];
        let overrides = NotionOverrides {
            team: Some("Squad".into()),
            role: Some("".into()),
            ..Default::default()
        };
        let mapping = resolve_mapping(&props, &overrides).unwrap();
        assert_eq!(mapping.team.as_deref(), Some("Squad"));
        assert_eq!(mapping.role, None, "an empty override unmaps the field");
        assert_eq!(mapping.projects.as_deref(), Some("Projects"));

        let missing = NotionOverrides {
            email: Some("Mail".into()),
            ..Default::default()
        };
        let error = resolve_mapping(&props, &missing).unwrap_err();
        assert!(error.contains("\"Mail\""), "{error}");

        let no_name = NotionOverrides {
            name: Some("".into()),
            ..Default::default()
        };
        assert!(resolve_mapping(&props, &no_name).is_err());
    }

    #[test]
    fn maps_property_types_to_text() {
        let related: RelatedPages = HashMap::new();
        let mapping = NotionMapping {
            name: Some("Name".into()),
            email: Some("Email".into()),
            role: Some("Role".into()),
            team: Some("Team".into()),
            projects: Some("Projects".into()),
        };
        let page = json!({
            "id": "p-1",
            "properties": {
                "Name": { "type": "title", "title": [{ "plain_text": "Ana " }, { "plain_text": "Lima" }] },
                "Email": { "type": "rich_text", "rich_text": rich("ana@example.com") },
                "Role": { "type": "formula", "formula": { "type": "string", "string": "Designer" } },
                "Team": { "type": "multi_select", "multi_select": [{ "name": "Design" }, { "name": "Growth" }] },
                "Projects": { "type": "multi_select", "multi_select": [{ "name": "Pricing" }, { "name": "pricing" }] }
            }
        });
        let input = page_to_input(&page, &mapping, &related).unwrap();
        assert_eq!(input.name, "Ana Lima");
        assert_eq!(input.email.as_deref(), Some("ana@example.com"));
        assert_eq!(input.role.as_deref(), Some("Designer"));
        assert_eq!(input.team.as_deref(), Some("Design, Growth"));
        assert_eq!(input.projects.len(), 1, "duplicate titles collapse");
        assert_eq!(input.projects[0].source, "notion");

        let nameless =
            json!({ "id": "p-2", "properties": { "Name": { "type": "title", "title": [] } } });
        assert!(page_to_input(&nameless, &mapping, &related).is_none());
    }

    #[tokio::test]
    async fn fetches_pages_across_cursors_and_resolves_relations() {
        let transport = FakeTransport::ok(vec![
            database_json(),
            query_page(
                vec![page("aa11", "Priya Shah", &["c0ffee01", "c0ffee02"])],
                Some("cur-2"),
            ),
            query_page(vec![page("bb22", "Dev Patel", &["c0ffee01"])], None),
            project_page("c0ffee01", "Offline mode"),
            project_page("c0ffee02", "Pricing"),
        ]);
        let client = NotionClient::new(&transport, "secret_token").with_pace(Duration::ZERO);
        let people = client
            .fetch_people("db-1", &NotionOverrides::default())
            .await
            .unwrap()
            .people;
        assert_eq!(people.len(), 2);
        let priya = &people[0];
        assert_eq!(priya.source, "notion");
        assert_eq!(priya.external_id, "aa11");
        assert_eq!(priya.name, "Priya Shah");
        assert_eq!(priya.email.as_deref(), Some("aa11@example.com"));
        assert_eq!(priya.role.as_deref(), Some("Engineer"));
        assert_eq!(priya.team.as_deref(), Some("Platform"));
        let titles: Vec<&str> = priya.projects.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, vec!["Offline mode", "Pricing"]);
        assert_eq!(
            priya.projects[0].url.as_deref(),
            Some("https://www.notion.so/c0ffee01")
        );
        assert_eq!(people[1].projects.len(), 1);

        let requests = transport.requests();
        assert_eq!(requests.len(), 5, "each related page is fetched once");
        assert_eq!(requests[0].url, "https://api.notion.com/v1/databases/db-1");
        assert_eq!(
            requests[1].url,
            "https://api.notion.com/v1/databases/db-1/query"
        );
        assert_eq!(requests[1].body.as_ref().unwrap().get("start_cursor"), None);
        assert_eq!(requests[2].body.as_ref().unwrap()["start_cursor"], "cur-2");
        assert_eq!(requests[3].url, "https://api.notion.com/v1/pages/c0ffee01");
        for request in &requests {
            assert!(request.headers.contains(&(
                "Authorization".to_string(),
                "Bearer secret_token".to_string()
            )));
            assert!(request
                .headers
                .contains(&("Notion-Version".to_string(), NOTION_VERSION.to_string())));
        }
    }

    #[tokio::test]
    async fn overrides_change_what_is_read() {
        let transport = FakeTransport::ok(vec![
            database_json(),
            query_page(vec![page("aa11", "Priya Shah", &["c0ffee01"])], None),
        ]);
        let overrides = NotionOverrides {
            projects: Some("Tags".into()),
            role: Some("".into()),
            ..Default::default()
        };
        let people = NotionClient::new(&transport, "t")
            .with_pace(Duration::ZERO)
            .fetch_people("db-1", &overrides)
            .await
            .unwrap()
            .people;
        let titles: Vec<&str> = people[0]
            .projects
            .iter()
            .map(|p| p.title.as_str())
            .collect();
        assert_eq!(titles, vec!["Mobile", "Web"]);
        assert_eq!(people[0].role, None);
        assert_eq!(transport.requests().len(), 2, "no relation lookups");
    }

    #[tokio::test]
    async fn a_failed_relation_lookup_keeps_the_people() {
        let transport = FakeTransport::new(vec![
            Ok(database_json()),
            Ok(query_page(
                vec![page("aa11", "Priya Shah", &["c0ffee01", "c0ffee02"])],
                None,
            )),
            Err(HttpError {
                status: Some(429),
                message: "rate limited".into(),
            }),
        ]);
        let fetched = NotionClient::new(&transport, "t")
            .with_pace(Duration::ZERO)
            .fetch_people("db-1", &NotionOverrides::default())
            .await
            .unwrap();
        assert_eq!(fetched.people.len(), 1);
        assert!(fetched.people[0].projects.is_empty());
        assert_eq!(fetched.warnings.len(), 1);
        assert!(
            fetched.warnings[0].contains("rate limiting"),
            "{:?}",
            fetched.warnings
        );
        assert_eq!(
            transport.requests().len(),
            3,
            "lookups stop after a failure"
        );
    }

    #[tokio::test]
    async fn lists_databases_with_detected_mappings() {
        let transport = FakeTransport::ok(vec![
            query_page(vec![database_json()], Some("next")),
            query_page(
                vec![
                    json!({ "object": "database", "id": "db-0", "title": [], "properties": {
                    "Name": { "name": "Name", "type": "title" } } }),
                ],
                None,
            ),
        ]);
        let databases = NotionClient::new(&transport, "t")
            .list_databases()
            .await
            .unwrap();
        assert_eq!(databases.len(), 2);
        assert_eq!(databases[0].title, "Team directory");
        assert_eq!(databases[0].detected.email.as_deref(), Some("Work email"));
        assert_eq!(databases[0].detected.team.as_deref(), Some("Department"));
        assert_eq!(databases[1].title, "Untitled");
        let first = transport.requests()[0].body.clone().unwrap();
        assert_eq!(first["filter"]["value"], "database");
        assert_eq!(
            transport.requests()[1].body.as_ref().unwrap()["start_cursor"],
            "next"
        );
    }

    #[tokio::test]
    async fn test_connection_reports_workspace_and_rows() {
        let transport = FakeTransport::ok(vec![
            json!({ "object": "user", "type": "bot", "bot": { "workspace_name": "Acme" } }),
            query_page(vec![page("aa11", "A", &[]), page("bb22", "B", &[])], None),
        ]);
        let result = NotionClient::new(&transport, "t")
            .test_connection(Some("db-1"))
            .await
            .unwrap();
        assert_eq!(result.workspace.as_deref(), Some("Acme"));
        assert_eq!(result.user_count, Some(2));
    }

    #[tokio::test]
    async fn errors_are_friendly_and_token_free() {
        let cases = [
            (Some(401), "Notion rejected the integration token"),
            (Some(404), "Notion can't find that database"),
            (Some(429), "rate limiting"),
            (None, "Could not reach Notion"),
        ];
        for (status, expected) in cases {
            let transport = FakeTransport::new(vec![Err(HttpError {
                status,
                message: "boom".into(),
            })]);
            let error = NotionClient::new(&transport, "secret_token")
                .fetch_people("db-1", &NotionOverrides::default())
                .await
                .unwrap_err();
            assert!(error.contains(expected), "{error}");
            assert!(!error.contains("secret_token"));
        }
    }

    #[test]
    fn ids_cannot_change_the_path() {
        assert_eq!(encode_id("12ab-34CD"), "12ab-34CD");
        assert_eq!(encode_id("../users/me?x"), "ee");
    }
}
