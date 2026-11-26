//! People from Linear, through its GraphQL API and a personal API key.
//!
//! Requests send the key and these read-only queries, nothing else: active
//! users (name, display name, email, teams, up to five in-progress assigned
//! issues) and projects (name, state, lead, members). Each active user becomes
//! a [`PersonInput`] with source `linear`.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::{json, Value};

use super::http::{graphql_error, HttpError, Method, Request, Transport};
use super::sync::ConnectionTest;
use super::{PersonInput, Project, SOURCE_LINEAR};

pub const LINEAR_API: &str = "https://api.linear.app/graphql";

/// In-progress assigned issues kept per person.
pub const MAX_ISSUES_PER_PERSON: usize = 5;

/// A runaway cursor must not loop forever.
const MAX_PAGES: usize = 200;

/// Project states that count as active.
const ACTIVE_PROJECT_STATES: [&str; 2] = ["started", "planned"];

pub const USERS_QUERY: &str = r#"query MinutesPeople($after: String) {
  users(first: 50, after: $after) {
    nodes {
      id
      name
      displayName
      email
      active
      teams(first: 20) { nodes { name } }
      assignedIssues(first: 5, orderBy: updatedAt, filter: { state: { type: { eq: "started" } } }) {
        nodes { title url }
      }
    }
    pageInfo { hasNextPage endCursor }
  }
}"#;

pub const PROJECTS_QUERY: &str = r#"query MinutesProjects($after: String) {
  projects(first: 25, after: $after) {
    nodes {
      id
      name
      url
      state
      lead { id }
      members(first: 50) { nodes { id } }
    }
    pageInfo { hasNextPage endCursor }
  }
}"#;

pub const ORGANIZATION_QUERY: &str = r#"query MinutesOrganization {
  organization { name }
}"#;

pub const USER_COUNT_QUERY: &str = r#"query MinutesUserCount($after: String) {
  users(first: 100, after: $after) {
    nodes { active }
    pageInfo { hasNextPage endCursor }
  }
}"#;

#[derive(Debug, Clone, Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection {
    nodes: Vec<Value>,
    page_info: PageInfo,
}

#[derive(Debug, Clone, Deserialize)]
struct IdOnly {
    id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Named {
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LinearIssue {
    pub title: String,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearUser {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default = "yes")]
    pub active: bool,
    #[serde(default)]
    teams: Option<Nodes<Named>>,
    #[serde(default)]
    assigned_issues: Option<Nodes<LinearIssue>>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct LinearProject {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    lead: Option<IdOnly>,
    #[serde(default)]
    members: Option<Nodes<IdOnly>>,
}

impl LinearProject {
    fn is_active(&self) -> bool {
        self.state
            .as_deref()
            .map(|s| ACTIVE_PROJECT_STATES.contains(&s.to_lowercase().as_str()))
            .unwrap_or(false)
    }

    fn people(&self) -> impl Iterator<Item = &str> {
        self.lead.iter().map(|l| l.id.as_str()).chain(
            self.members
                .iter()
                .flat_map(|m| m.nodes.iter().map(|n| n.id.as_str())),
        )
    }
}

/// A user-facing message for a failed Linear request. Never includes the key.
pub fn describe_error(error: &HttpError) -> String {
    let lower = error.message.to_lowercase();
    match error.status {
        None => format!("Could not reach Linear: {}", error.message),
        Some(401) | Some(403) => "Linear rejected the API key".to_string(),
        Some(_) if lower.contains("authenticat") => "Linear rejected the API key".to_string(),
        Some(429) => "Linear is rate limiting requests; try again later".to_string(),
        Some(200) => format!("Linear returned an error: {}", error.message),
        Some(status) => format!("Linear returned an error ({status}): {}", error.message),
    }
}

pub struct LinearClient<'a, T: Transport> {
    transport: &'a T,
    token: &'a str,
}

impl<'a, T: Transport> LinearClient<'a, T> {
    pub fn new(transport: &'a T, token: &'a str) -> Self {
        Self { transport, token }
    }

    fn request(&self, query: &str, variables: Value) -> Request {
        Request {
            method: Method::Post,
            url: LINEAR_API.to_string(),
            // Personal API keys go in the header as-is (no "Bearer").
            headers: vec![
                ("Authorization".to_string(), self.token.trim().to_string()),
                ("Content-Type".to_string(), "application/json".to_string()),
            ],
            body: Some(json!({ "query": query, "variables": variables })),
        }
    }

    async fn graphql(&self, query: &str, variables: Value) -> Result<Value, String> {
        let response = self
            .transport
            .send(self.request(query, variables))
            .await
            .map_err(|e| describe_error(&e))?;
        if let Some(message) = graphql_error(&response) {
            return Err(describe_error(&HttpError {
                status: Some(200),
                message,
            }));
        }
        response
            .get("data")
            .cloned()
            .ok_or_else(|| "Linear returned no data".to_string())
    }

    /// Every node of a paginated top-level connection.
    async fn paginate(&self, query: &str, field: &str) -> Result<Vec<Value>, String> {
        let mut nodes = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let data = self.graphql(query, json!({ "after": after })).await?;
            let connection: Connection = data
                .get(field)
                .cloned()
                .ok_or_else(|| format!("Linear returned no {field}"))
                .and_then(|v| {
                    serde_json::from_value(v)
                        .map_err(|e| format!("Unexpected Linear response for {field}: {e}"))
                })?;
            nodes.extend(connection.nodes);
            match (
                connection.page_info.has_next_page,
                connection.page_info.end_cursor,
            ) {
                (true, Some(cursor)) => after = Some(cursor),
                _ => return Ok(nodes),
            }
        }
        Ok(nodes)
    }

    pub async fn users(&self) -> Result<Vec<LinearUser>, String> {
        self.paginate(USERS_QUERY, "users")
            .await?
            .into_iter()
            .map(|v| serde_json::from_value(v).map_err(|e| format!("Unexpected Linear user: {e}")))
            .collect()
    }

    pub async fn projects(&self) -> Result<Vec<LinearProject>, String> {
        self.paginate(PROJECTS_QUERY, "projects")
            .await?
            .into_iter()
            .map(|v| {
                serde_json::from_value(v).map_err(|e| format!("Unexpected Linear project: {e}"))
            })
            .collect()
    }

    /// Every active user as a [`PersonInput`].
    pub async fn fetch_people(&self) -> Result<Vec<PersonInput>, String> {
        let users = self.users().await?;
        let projects = self.projects().await?;
        Ok(to_inputs(&users, &projects))
    }

    /// The workspace name and how many active users it has.
    pub async fn test_connection(&self) -> Result<ConnectionTest, String> {
        let data = self.graphql(ORGANIZATION_QUERY, json!({})).await?;
        let workspace = data
            .pointer("/organization/name")
            .and_then(Value::as_str)
            .map(str::to_string);
        let users = self.paginate(USER_COUNT_QUERY, "users").await?;
        let active = users
            .iter()
            .filter(|u| u.get("active").and_then(Value::as_bool).unwrap_or(true))
            .count();
        Ok(ConnectionTest {
            workspace,
            user_count: Some(active),
        })
    }
}

/// Maps Linear users (and the projects they lead or belong to) to people.
/// Inactive users are skipped.
pub fn to_inputs(users: &[LinearUser], projects: &[LinearProject]) -> Vec<PersonInput> {
    let mut by_user: HashMap<&str, Vec<&LinearProject>> = HashMap::new();
    for project in projects.iter().filter(|p| p.is_active()) {
        let mut seen = HashSet::new();
        for user in project.people() {
            if seen.insert(user) {
                by_user.entry(user).or_default().push(project);
            }
        }
    }

    users
        .iter()
        .filter(|u| u.active)
        .map(|user| {
            let name = user.name.trim().to_string();
            let aliases = user
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty() && !d.eq_ignore_ascii_case(&name))
                .map(|d| vec![d.to_string()])
                .unwrap_or_default();
            let teams: Vec<String> = user
                .teams
                .iter()
                .flat_map(|t| t.nodes.iter().map(|n| n.name.trim().to_string()))
                .filter(|n| !n.is_empty())
                .collect();
            let team = (!teams.is_empty()).then(|| teams.join(", "));

            let mut titles = HashSet::new();
            let mut list = Vec::new();
            for project in by_user.get(user.id.as_str()).into_iter().flatten() {
                if titles.insert(project.name.to_lowercase()) {
                    list.push(Project {
                        title: project.name.clone(),
                        url: project.url.clone(),
                        source: SOURCE_LINEAR.to_string(),
                    });
                }
            }
            for issue in user
                .assigned_issues
                .iter()
                .flat_map(|i| i.nodes.iter())
                .take(MAX_ISSUES_PER_PERSON)
            {
                if titles.insert(issue.title.to_lowercase()) {
                    list.push(Project {
                        title: issue.title.clone(),
                        url: issue.url.clone(),
                        source: SOURCE_LINEAR.to_string(),
                    });
                }
            }

            PersonInput {
                source: SOURCE_LINEAR.to_string(),
                external_id: user.id.clone(),
                name,
                email: user.email.clone(),
                role: None,
                team,
                aliases,
                projects: list,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::people::http::fake::FakeTransport;

    fn users_page(nodes: Value, next: Option<&str>) -> Value {
        json!({ "data": { "users": {
            "nodes": nodes,
            "pageInfo": { "hasNextPage": next.is_some(), "endCursor": next }
        }}})
    }

    fn projects_page(nodes: Value, next: Option<&str>) -> Value {
        json!({ "data": { "projects": {
            "nodes": nodes,
            "pageInfo": { "hasNextPage": next.is_some(), "endCursor": next }
        }}})
    }

    fn priya() -> Value {
        json!({
            "id": "u-priya",
            "name": "Priya Shah",
            "displayName": "priya",
            "email": "Priya@Example.com",
            "active": true,
            "teams": { "nodes": [{ "name": "Growth" }, { "name": "Platform" }] },
            "assignedIssues": { "nodes": [
                { "title": "Fix pricing page", "url": "https://linear.app/x/issue/GRO-1" },
                { "title": "Offline mode", "url": "https://linear.app/x/issue/GRO-2" },
                { "title": "Three", "url": null },
                { "title": "Four" },
                { "title": "Five" },
                { "title": "Six" }
            ]}
        })
    }

    fn dev() -> Value {
        json!({
            "id": "u-dev",
            "name": "Dev Patel",
            "displayName": "Dev Patel",
            "email": "dev@example.com",
            "active": true,
            "teams": { "nodes": [] },
            "assignedIssues": { "nodes": [] }
        })
    }

    fn gone() -> Value {
        json!({ "id": "u-gone", "name": "Former", "email": "f@example.com", "active": false })
    }

    fn projects() -> Value {
        json!([
            { "id": "p1", "name": "Offline mode", "url": "https://linear.app/x/project/offline",
              "state": "started", "lead": { "id": "u-priya" },
              "members": { "nodes": [{ "id": "u-priya" }, { "id": "u-dev" }] } },
            { "id": "p2", "name": "Pricing", "url": null, "state": "planned", "lead": null,
              "members": { "nodes": [{ "id": "u-dev" }] } },
            { "id": "p3", "name": "Old launch", "state": "completed", "lead": { "id": "u-priya" },
              "members": { "nodes": [] } }
        ])
    }

    #[tokio::test]
    async fn maps_users_teams_projects_and_issues() {
        let transport = FakeTransport::ok(vec![
            users_page(json!([priya(), dev(), gone()]), None),
            projects_page(projects(), None),
        ]);
        let people = LinearClient::new(&transport, "lin_api_secret")
            .fetch_people()
            .await
            .unwrap();
        assert_eq!(people.len(), 2, "inactive users are skipped");

        let priya = &people[0];
        assert_eq!(priya.source, "linear");
        assert_eq!(priya.external_id, "u-priya");
        assert_eq!(priya.name, "Priya Shah");
        assert_eq!(priya.email.as_deref(), Some("Priya@Example.com"));
        assert_eq!(priya.aliases, vec!["priya".to_string()]);
        assert_eq!(priya.team.as_deref(), Some("Growth, Platform"));
        let titles: Vec<&str> = priya.projects.iter().map(|p| p.title.as_str()).collect();
        // The project first; the duplicate issue title is dropped; issues are
        // capped at five; the completed project is left out.
        assert_eq!(
            titles,
            vec!["Offline mode", "Fix pricing page", "Three", "Four", "Five"]
        );
        assert_eq!(
            priya.projects[0].url.as_deref(),
            Some("https://linear.app/x/project/offline")
        );
        assert_eq!(
            priya.projects[1].url.as_deref(),
            Some("https://linear.app/x/issue/GRO-1")
        );
        assert!(priya.projects.iter().all(|p| p.source == "linear"));

        let dev = &people[1];
        assert!(dev.aliases.is_empty(), "same display name is not an alias");
        assert_eq!(dev.team, None);
        let titles: Vec<&str> = dev.projects.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, vec!["Offline mode", "Pricing"]);
    }

    #[tokio::test]
    async fn follows_cursors_and_sends_only_the_key_and_query() {
        let transport = FakeTransport::ok(vec![
            users_page(json!([priya()]), Some("cursor-1")),
            users_page(json!([dev()]), None),
            projects_page(json!([]), Some("cursor-p")),
            projects_page(projects(), None),
        ]);
        let people = LinearClient::new(&transport, " lin_api_secret ")
            .fetch_people()
            .await
            .unwrap();
        assert_eq!(people.len(), 2);
        assert_eq!(people[1].projects.len(), 2);

        let requests = transport.requests();
        assert_eq!(requests.len(), 4);
        assert!(requests
            .iter()
            .all(|r| r.url == LINEAR_API && r.method == Method::Post));
        assert!(requests.iter().all(|r| r
            .headers
            .contains(&("Authorization".to_string(), "lin_api_secret".to_string()))));
        assert_eq!(
            requests[0].body.as_ref().unwrap()["variables"]["after"],
            Value::Null
        );
        assert_eq!(
            requests[1].body.as_ref().unwrap()["variables"]["after"],
            "cursor-1"
        );
        assert_eq!(
            requests[3].body.as_ref().unwrap()["variables"]["after"],
            "cursor-p"
        );
        for request in &requests {
            let body = request.body.as_ref().unwrap().as_object().unwrap();
            let mut keys: Vec<&str> = body.keys().map(String::as_str).collect();
            keys.sort();
            assert_eq!(keys, vec!["query", "variables"]);
        }
    }

    #[tokio::test]
    async fn test_connection_reports_workspace_and_active_users() {
        let transport = FakeTransport::ok(vec![
            json!({ "data": { "organization": { "name": "Acme" } } }),
            users_page(json!([{ "active": true }, { "active": false }]), Some("c")),
            users_page(json!([{ "active": true }]), None),
        ]);
        let result = LinearClient::new(&transport, "k")
            .test_connection()
            .await
            .unwrap();
        assert_eq!(result.workspace.as_deref(), Some("Acme"));
        assert_eq!(result.user_count, Some(2));
    }

    #[tokio::test]
    async fn reports_errors_without_the_key() {
        let rejected = FakeTransport::new(vec![Err(HttpError {
            status: Some(400),
            message: "Authentication required, not authenticated".into(),
        })]);
        let error = LinearClient::new(&rejected, "lin_api_secret")
            .fetch_people()
            .await
            .unwrap_err();
        assert_eq!(error, "Linear rejected the API key");

        let offline = FakeTransport::new(vec![Err(HttpError {
            status: None,
            message: "the request timed out".into(),
        })]);
        let error = LinearClient::new(&offline, "lin_api_secret")
            .fetch_people()
            .await
            .unwrap_err();
        assert_eq!(error, "Could not reach Linear: the request timed out");

        let graphql = FakeTransport::ok(vec![json!({
            "errors": [{ "message": "Query too complex" }]
        })]);
        let error = LinearClient::new(&graphql, "lin_api_secret")
            .fetch_people()
            .await
            .unwrap_err();
        assert!(error.contains("Query too complex"), "{error}");
        assert!(!error.contains("lin_api_secret"));

        let limited = FakeTransport::new(vec![Err(HttpError {
            status: Some(429),
            message: "slow down".into(),
        })]);
        let error = LinearClient::new(&limited, "k")
            .fetch_people()
            .await
            .unwrap_err();
        assert!(error.contains("rate limiting"));
    }

    #[tokio::test]
    async fn malformed_responses_are_errors() {
        let transport =
            FakeTransport::ok(vec![json!({ "data": { "users": { "nodes": "nope" } } })]);
        assert!(LinearClient::new(&transport, "k")
            .fetch_people()
            .await
            .is_err());
    }
}
