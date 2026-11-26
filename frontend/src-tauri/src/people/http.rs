//! The JSON-over-HTTPS transport the Linear and Notion syncs share.
//!
//! The syncs talk to a [`Transport`] rather than to reqwest directly, so tests
//! can feed fixture responses without a network. Requests carry the user's
//! token in a header and nothing else about the user; the token never appears
//! in an error or a log line.

use std::future::Future;
use std::time::Duration;

use serde_json::Value;

/// Every people-sync request gives up after this long.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Value>,
}

/// Why a request failed. `status` is `None` when the server was never reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpError {
    pub status: Option<u16>,
    pub message: String,
}

pub trait Transport: Send + Sync {
    fn send(&self, request: Request) -> impl Future<Output = Result<Value, HttpError>> + Send;
}

impl<T: Transport> Transport for &T {
    fn send(&self, request: Request) -> impl Future<Output = Result<Value, HttpError>> + Send {
        (**self).send(request)
    }
}

/// The real transport: reqwest with a 20 second timeout.
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .user_agent(concat!(
                "Minutes/",
                env!("CARGO_PKG_VERSION"),
                " (people-sync)"
            ))
            .build()
            .map_err(|error| format!("Failed to create HTTP client: {error}"))?;
        Ok(Self { client })
    }
}

/// A short description of a transport failure, without the URL.
fn describe(error: reqwest::Error) -> String {
    if error.is_timeout() {
        "the request timed out".to_string()
    } else if error.is_connect() {
        "could not connect".to_string()
    } else {
        error.without_url().to_string()
    }
}

impl Transport for ReqwestTransport {
    async fn send(&self, request: Request) -> Result<Value, HttpError> {
        let mut builder = match request.method {
            Method::Get => self.client.get(&request.url),
            Method::Post => self.client.post(&request.url),
        };
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = &request.body {
            builder = builder.json(body);
        }
        let response = builder.send().await.map_err(|error| HttpError {
            status: None,
            message: describe(error),
        })?;
        let status = response.status();
        let text = response.text().await.map_err(|error| HttpError {
            status: Some(status.as_u16()),
            message: describe(error),
        })?;
        if !status.is_success() {
            return Err(HttpError {
                status: Some(status.as_u16()),
                message: error_message_from_body(&text),
            });
        }
        serde_json::from_str(&text).map_err(|error| HttpError {
            status: Some(status.as_u16()),
            message: format!("unexpected response: {error}"),
        })
    }
}

/// The human-readable message in a JSON error body (Notion's `message`,
/// GraphQL's `errors[0].message`), or a trimmed slice of the body.
pub fn error_message_from_body(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        if let Some(message) = value.get("message").and_then(Value::as_str) {
            return message.to_string();
        }
        if let Some(message) = graphql_error(&value) {
            return message;
        }
    }
    body.chars().take(200).collect()
}

/// The first GraphQL error message in a response, if any.
pub fn graphql_error(value: &Value) -> Option<String> {
    value
        .get("errors")
        .and_then(Value::as_array)
        .and_then(|errors| errors.first())
        .map(|error| {
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string()
        })
}

#[cfg(test)]
pub mod fake {
    //! A transport that replays queued responses and records requests.

    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct FakeTransport {
        responses: Mutex<VecDeque<Result<Value, HttpError>>>,
        pub requests: Mutex<Vec<Request>>,
    }

    impl FakeTransport {
        pub fn new(responses: Vec<Result<Value, HttpError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                requests: Mutex::new(Vec::new()),
            }
        }

        pub fn ok(responses: Vec<Value>) -> Self {
            Self::new(responses.into_iter().map(Ok).collect())
        }

        pub fn requests(&self) -> Vec<Request> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Transport for FakeTransport {
        async fn send(&self, request: Request) -> Result<Value, HttpError> {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| {
                    Err(HttpError {
                        status: None,
                        message: "no more fake responses".to_string(),
                    })
                })
        }
    }
}
