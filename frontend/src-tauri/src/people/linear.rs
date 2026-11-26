//! People from Linear, through its GraphQL API and a personal API key.
//!
//! Requests send the key and these read-only queries, nothing else: active
//! users (name, display name, email, teams), their in-progress assigned issues
//! (up to five each) and projects (name, status, lead, members). Each active
//! user becomes a [`PersonInput`] with source `linear`.
//!
//! Only the users query can fail a sync. Issues and projects are separate
//! queries: if Linear rejects one, that run goes without them and the sync
//! carries a warning instead.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::{json, Value};

use super::http::{graphql_error, HttpError, Method, Request, Transport};
use super::sync::{ConnectionTest, Fetched};
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
    }
    pageInfo { hasNextPage endCursor }
  }
}"#;

/// Each user's in-progress assigned issues, most recently updated first.
pub const ISSUES_QUERY: &str = r#"query MinutesIssues($after: String) {
  users(first: 50, after: $after) {
    nodes {
      id
      assignedIssues(first: 5, orderBy: updatedAt, filter: { state: { type: { eq: "started" } } }) {
        nodes { title url }
      }
    }
    pageInfo { hasNextPage endCursor }
  }
}"#;

/// [`ISSUES_QUERY`] without the ordering, in case Linear rejects it.
pub const ISSUES_QUERY_UNORDERED: &str = r#"query MinutesIssues($after: String) {
  users(first: 50, after: $after) {
    nodes {
      id
      assignedIssues(first: 5, filter: { state: { type: { eq: "started" } } }) {
        nodes { title url }
      }
    }
    pageInfo { hasNextPage endCursor }
  }
}"#;

/// Projects with their current `status`.
pub const PROJECTS_QUERY: &str = r#"query MinutesProjects($after: String) {
  projects(first: 25, after: $after) {
    nodes {
      id
      name
      url
      status { type }
      lead { id }
      members(first: 50) { nodes { id } }
    }
    pageInfo { hasNextPage endCursor }
  }
}"#;

/// [`PROJECTS_QUERY`] with the older, deprecated `state` field, for when
/// `status` isn't available.
pub const PROJECTS_QUERY_LEGACY: &str = r#"query MinutesProjects($after: String) {
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
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserIssues {
    id: String,
    #[serde(default)]
    assigned_issues: Option<Nodes<LinearIssue>>,
}

/// In-progress assigned issues by Linear user id.
pub type IssuesByUser = HashMap<String, Vec<LinearIssue>>;

#[derive(Debug, Clone, Deserialize)]
struct ProjectStatus {
    #[serde(rename = "type", default)]
    kind: Option<String>,
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
    status: Option<ProjectStatus>,
    /// Deprecated in Linear; read only when `status` isn't available.
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    lead: Option<IdOnly>,
    #[serde(default)]
    members: Option<Nodes<IdOnly>>,
}

impl LinearProject {
    fn is_active(&self) -> bool {
        self.status
            .as_ref()
            .and_then(|s| s.kind.as_deref())
            .or(self.state.as_deref())
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

    async fn projects_with(&self, query: &str) -> Result<Vec<LinearProject>, String> {
        self.paginate(query, "projects")
            .await?
            .into_iter()
            .map(|v| {
                serde_json::from_value(v).map_err(|e| format!("Unexpected Linear project: {e}"))
            })
            .collect()
    }

    /// Projects, read with `status`, or with the older `state` when Linear
    /// rejects `status`. The error is the first attempt's.
    pub async fn projects(&self) -> Result<Vec<LinearProject>, String> {
        match self.projects_with(PROJECTS_QUERY).await {
            Ok(projects) => Ok(projects),
            Err(first) => {
                log::warn!("Linear projects query failed ({first}); retrying with `state`");
                self.projects_with(PROJECTS_QUERY_LEGACY)
                    .await
                    .map_err(|_| first)
            }
        }
    }

    async fn issues_with(&self, query: &str) -> Result<IssuesByUser, String> {
        let mut by_user = IssuesByUser::new();
        for node in self.paginate(query, "users").await? {
            let user: UserIssues = serde_json::from_value(node)
                .map_err(|e| format!("Unexpected Linear issues: {e}"))?;
            let issues: Vec<LinearIssue> = user
                .assigned_issues
                .map(|i| i.nodes)
                .unwrap_or_default()
                .into_iter()
                .take(MAX_ISSUES_PER_PERSON)
                .collect();
            if !issues.is_empty() {
                by_user.insert(user.id, issues);
            }
        }
        Ok(by_user)
    }

    /// In-progress assigned issues per user, ordered by last update when
    /// Linear allows it.
    pub async fn issues(&self) -> Result<IssuesByUser, String> {
        match self.issues_with(ISSUES_QUERY).await {
            Ok(issues) => Ok(issues),
            Err(first) => {
                log::warn!("Linear issues query failed ({first}); retrying unordered");
                self.issues_with(ISSUES_QUERY_UNORDERED)
                    .await
                    .map_err(|_| first)
            }
        }
    }

    /// Every active user as a [`PersonInput`]. Only the users query is
    /// required: failed issue or project queries become warnings.
    pub async fn fetch_people(&self) -> Result<Fetched, String> {
        let users = self.users().await?;
        let mut warnings = Vec::new();
        let issues = match self.issues().await {
            Ok(issues) => issues,
            Err(error) => {
                log::warn!("Linear sync continues without issues: {error}");
                warnings.push(format!("Issues couldn't be read: {error}"));
                IssuesByUser::new()
            }
        };
        let projects = match self.projects().await {
            Ok(projects) => projects,
            Err(error) => {
                log::warn!("Linear sync continues without projects: {error}");
                warnings.push(format!("Projects couldn't be read: {error}"));
                Vec::new()
            }
        };
        Ok(Fetched {
            people: to_inputs(&users, &projects, &issues),
            warnings,
        })
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
pub fn to_inputs(
    users: &[LinearUser],
    projects: &[LinearProject],
    issues: &IssuesByUser,
) -> Vec<PersonInput> {
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
            for issue in issues
                .get(&user.id)
                .into_iter()
                .flatten()
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
            "teams": { "nodes": [{ "name": "Growth" }, { "name": "Platform" }] }
        })
    }

    fn dev() -> Value {
        json!({
            "id": "u-dev",
            "name": "Dev Patel",
            "displayName": "Dev Patel",
            "email": "dev@example.com",
            "active": true,
            "teams": { "nodes": [] }
        })
    }

    fn gone() -> Value {
        json!({ "id": "u-gone", "name": "Former", "email": "f@example.com", "active": false })
    }

    /// The issues query's page: users with their assigned issues.
    fn issues() -> Value {
        json!([
            { "id": "u-priya", "assignedIssues": { "nodes": [
                { "title": "Fix pricing page", "url": "https://linear.app/x/issue/GRO-1" },
                { "title": "Offline mode", "url": "https://linear.app/x/issue/GRO-2" },
                { "title": "Three", "url": null },
                { "title": "Four" },
                { "title": "Five" },
                { "title": "Six" }
            ]}},
            { "id": "u-dev", "assignedIssues": { "nodes": [] } }
        ])
    }

    fn projects() -> Value {
        json!([
            { "id": "p1", "name": "Offline mode", "url": "https://linear.app/x/project/offline",
              "status": { "type": "started" }, "lead": { "id": "u-priya" },
              "members": { "nodes": [{ "id": "u-priya" }, { "id": "u-dev" }] } },
            { "id": "p2", "name": "Pricing", "url": null, "status": { "type": "planned" }, "lead": null,
              "members": { "nodes": [{ "id": "u-dev" }] } },
            { "id": "p3", "name": "Old launch", "status": { "type": "completed" }, "lead": { "id": "u-priya" },
              "members": { "nodes": [] } }
        ])
    }

    fn graphql_error(message: &str) -> Value {
        json!({ "errors": [{ "message": message }] })
    }

    fn query_of(request: &Request) -> String {
        request.body.as_ref().unwrap()["query"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn maps_users_teams_projects_and_issues() {
        let transport = FakeTransport::ok(vec![
            users_page(json!([priya(), dev(), gone()]), None),
            users_page(issues(), None),
            projects_page(projects(), None),
        ]);
        let fetched = LinearClient::new(&transport, "lin_api_secret")
            .fetch_people()
            .await
            .unwrap();
        assert!(fetched.warnings.is_empty());
        let people = fetched.people;
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
            users_page(json!([]), Some("cursor-i")),
            users_page(issues(), None),
            projects_page(json!([]), Some("cursor-p")),
            projects_page(projects(), None),
        ]);
        let people = LinearClient::new(&transport, " lin_api_secret ")
            .fetch_people()
            .await
            .unwrap()
            .people;
        assert_eq!(people.len(), 2);
        assert_eq!(people[1].projects.len(), 2);
        assert_eq!(people[0].projects.len(), 5);

        let requests = transport.requests();
        assert_eq!(requests.len(), 6);
        assert!(requests
            .iter()
            .all(|r| r.url == LINEAR_API && r.method == Method::Post));
        assert!(requests.iter().all(|r| r
            .headers
            .contains(&("Authorization".to_string(), "lin_api_secret".to_string()))));
        let after = |i: usize| requests[i].body.as_ref().unwrap()["variables"]["after"].clone();
        assert_eq!(after(0), Value::Null);
        assert_eq!(after(1), "cursor-1");
        assert_eq!(after(3), "cursor-i");
        assert_eq!(after(5), "cursor-p");
        assert_eq!(query_of(&requests[2]), ISSUES_QUERY);
        assert_eq!(query_of(&requests[4]), PROJECTS_QUERY);
        for request in &requests {
            let body = request.body.as_ref().unwrap().as_object().unwrap();
            let mut keys: Vec<&str> = body.keys().map(String::as_str).collect();
            keys.sort();
            assert_eq!(keys, vec!["query", "variables"]);
        }
    }

    #[tokio::test]
    async fn falls_back_to_legacy_project_state() {
        let legacy = json!([
            { "id": "p1", "name": "Offline mode", "state": "started",
              "lead": { "id": "u-dev" }, "members": { "nodes": [] } },
            { "id": "p2", "name": "Done thing", "state": "completed",
              "lead": { "id": "u-dev" }, "members": { "nodes": [] } }
        ]);
        let transport = FakeTransport::ok(vec![
            users_page(json!([dev()]), None),
            users_page(json!([]), None),
            graphql_error("Cannot query field \"status\" on type \"Project\"."),
            projects_page(legacy, None),
        ]);
        let fetched = LinearClient::new(&transport, "k")
            .fetch_people()
            .await
            .unwrap();
        assert!(fetched.warnings.is_empty());
        let titles: Vec<&str> = fetched.people[0]
            .projects
            .iter()
            .map(|p| p.title.as_str())
            .collect();
        assert_eq!(titles, vec!["Offline mode"]);
        assert_eq!(query_of(&transport.requests()[3]), PROJECTS_QUERY_LEGACY);
    }

    #[tokio::test]
    async fn a_failed_projects_query_keeps_people_and_issues() {
        let transport = FakeTransport::ok(vec![
            users_page(json!([priya(), dev()]), None),
            users_page(issues(), None),
            graphql_error("Cannot query field \"status\" on type \"Project\"."),
            graphql_error("Field \"state\" is not available."),
        ]);
        let fetched = LinearClient::new(&transport, "k")
            .fetch_people()
            .await
            .unwrap();
        assert_eq!(fetched.people.len(), 2);
        assert_eq!(fetched.people[0].team.as_deref(), Some("Growth, Platform"));
        let titles: Vec<&str> = fetched.people[0]
            .projects
            .iter()
            .map(|p| p.title.as_str())
            .collect();
        assert_eq!(
            titles,
            vec!["Fix pricing page", "Offline mode", "Three", "Four", "Five"],
            "issues survive without projects"
        );
        assert_eq!(fetched.warnings.len(), 1);
        assert!(
            fetched.warnings[0].starts_with("Projects couldn't be read: "),
            "{:?}",
            fetched.warnings
        );
        assert!(
            fetched.warnings[0].contains("status"),
            "reports the first error"
        );
    }

    #[tokio::test]
    async fn a_failed_issues_query_keeps_people_and_projects() {
        let transport = FakeTransport::ok(vec![
            users_page(json!([dev()]), None),
            graphql_error("Unknown argument \"orderBy\""),
            graphql_error("Unknown argument \"filter\""),
            projects_page(projects(), None),
        ]);
        let fetched = LinearClient::new(&transport, "k")
            .fetch_people()
            .await
            .unwrap();
        assert_eq!(fetched.people.len(), 1);
        assert_eq!(fetched.people[0].projects.len(), 2);
        assert_eq!(fetched.warnings.len(), 1);
        assert!(fetched.warnings[0].starts_with("Issues couldn't be read: "));
        let requests = transport.requests();
        assert_eq!(query_of(&requests[1]), ISSUES_QUERY);
        assert_eq!(query_of(&requests[2]), ISSUES_QUERY_UNORDERED);
    }

    #[tokio::test]
    async fn unordered_issues_are_used_when_ordering_is_rejected() {
        let transport = FakeTransport::ok(vec![
            users_page(json!([priya()]), None),
            graphql_error("Unknown argument \"orderBy\""),
            users_page(issues(), None),
            projects_page(json!([]), None),
        ]);
        let fetched = LinearClient::new(&transport, "k")
            .fetch_people()
            .await
            .unwrap();
        assert!(fetched.warnings.is_empty());
        assert_eq!(fetched.people[0].projects.len(), 5);
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
