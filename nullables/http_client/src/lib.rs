#![no_std]
extern crate alloc;

pub mod dns;
pub mod tcp_client;

use anyhow::anyhow;
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use serde::{de::DeserializeOwned, Serialize};
use alloc::{string::String, vec::Vec, sync::Arc};
use hashbrown::HashMap;
use core::future::poll_fn;
use core::task::Poll;
use reqwless::request::RequestBuilder;

pub use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    GET,
    POST,
    PUT,
    DELETE,
    PATCH,
    HEAD,
    OPTIONS,
}

impl core::fmt::Display for Method {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            Method::GET => "GET",
            Method::POST => "POST",
            Method::PUT => "PUT",
            Method::DELETE => "DELETE",
            Method::PATCH => "PATCH",
            Method::HEAD => "HEAD",
            Method::OPTIONS => "OPTIONS",
        };
        write!(f, "{}", s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StatusCode(u16);

impl StatusCode {
    pub const OK: StatusCode = StatusCode(200);
    pub const INTERNAL_SERVER_ERROR: StatusCode = StatusCode(500);

    pub fn from_u16(code: u16) -> Result<Self, u16> {
        Ok(StatusCode(code))
    }

    pub fn is_client_error(&self) -> bool {
        self.0 >= 400 && self.0 < 500
    }

    pub fn is_server_error(&self) -> bool {
        self.0 >= 500 && self.0 < 600
    }

    pub fn as_u16(&self) -> u16 {
        self.0
    }
}

impl core::fmt::Display for StatusCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// Mock IntoUrl for no_std
pub trait IntoUrl {
    fn into_url(self) -> anyhow::Result<Url>;
}

impl IntoUrl for Url {
    fn into_url(self) -> anyhow::Result<Url> {
        Ok(self)
    }
}

impl IntoUrl for String {
    fn into_url(self) -> anyhow::Result<Url> {
        self.parse().map_err(|e| anyhow!("Invalid URL: {}", e))
    }
}

impl IntoUrl for &str {
    fn into_url(self) -> anyhow::Result<Url> {
        self.parse().map_err(|e| anyhow!("Invalid URL: {}", e))
    }
}

pub struct HttpClient {
    strategy: HttpClientStrategy,
    request_listener: OutputListenerMt<TrackedRequest>,
}

impl HttpClient {
    pub fn new() -> Self {
        Self::new_with_strategy(HttpClientStrategy::Real)
    }

    pub fn new_null() -> Self {
        Self::new_with_strategy(HttpClientStrategy::Nulled(HttpClientStub::with_response(
            ConfiguredHttpResponse::Json(JsonResponse::default()),
        )))
    }

    fn new_with_strategy(strategy: HttpClientStrategy) -> Self {
        Self {
            strategy,
            request_listener: OutputListenerMt::new(),
        }
    }

    pub fn null_builder() -> NulledHttpClientBuilder {
        NulledHttpClientBuilder {
            responses: HashMap::new(),
        }
    }

    pub async fn post_json<U: IntoUrl, T: Serialize + ?Sized>(
        &self,
        url: U,
        json: &T,
    ) -> anyhow::Result<Response> {
        let url = url.into_url()?;
        if self.request_listener.is_tracked() {
            self.request_listener.emit(TrackedRequest {
                url: url.clone(),
                method: Method::POST,
                json: serde_json::to_value(json)?,
            });
        }

        match &self.strategy {
            HttpClientStrategy::Real => {
                let mut dns = dns::AsyncDns;
                let tcp_client = tcp_client::AsyncTcpClient;
                let mut client = reqwless::client::HttpClient::new(&tcp_client, &mut dns);
                
                let json_string = serde_json::to_string(json)?;
                let mut rx_buf = [0; 4096];
                
                let req_method = match Method::POST {
                    Method::GET => reqwless::request::Method::GET,
                    Method::POST => reqwless::request::Method::POST,
                    _ => unimplemented!(),
                };
                
                let request = client
                    .request(req_method, url.as_str())
                    .await
                    .map_err(|_| anyhow!("Failed to connect"))?;
                
                let request = request.body(json_string.as_bytes());
                let mut request = request.content_type(reqwless::headers::ContentType::ApplicationJson);
                
                let response = request
                    .send(&mut rx_buf)
                    .await
                    .map_err(|_| anyhow!("Request failed"))?;
                    
                let status_code = response.status.0;
                let body_bytes = response.body().read_to_end().await.map_err(|_| anyhow!("Body read error"))?.to_vec();
                
                let mut resp_str = String::new();
                for b in body_bytes {
                    resp_str.push(b as char);
                }

                Ok(Response {
                    strategy: ResponseStrategy::Real(RealResponse {
                        status: StatusCode::from_u16(status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                        body: resp_str,
                    })
                })
            }
            HttpClientStrategy::Nulled(client) => client.get_response(Method::POST, url).await,
        }
    }

    pub fn track_requests(&self) -> Arc<OutputTrackerMt<TrackedRequest>> {
        self.request_listener.track()
    }
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

enum HttpClientStrategy {
    Real,
    Nulled(HttpClientStub),
}

pub struct NulledHttpClientBuilder {
    responses: HashMap<(Url, Method), ConfiguredHttpResponse>,
}

impl NulledHttpClientBuilder {
    pub fn respond(self, response: JsonResponse) -> HttpClient {
        Self::create_client_with_response(ConfiguredHttpResponse::Json(response))
    }

    pub fn respond_url(
        mut self,
        method: Method,
        url: impl IntoUrl,
        response: ConfiguredHttpResponse,
    ) -> Self {
        self.responses
            .insert((url.into_url().unwrap(), method), response);
        self
    }

    pub fn fail_with(self, error_message: impl Into<String>) -> HttpClient {
        Self::create_client_with_response(ConfiguredHttpResponse::Error(error_message.into()))
    }

    pub fn finish(self) -> HttpClient {
        Self::create_client(HttpClientStub {
            responses: self.responses,
            ..Default::default()
        })
    }

    pub fn halt(self) -> HttpClient {
        Self::create_client_with_response(ConfiguredHttpResponse::Halt)
    }

    fn create_client_with_response(response: ConfiguredHttpResponse) -> HttpClient {
        Self::create_client(HttpClientStub::with_response(response))
    }

    fn create_client(stub: HttpClientStub) -> HttpClient {
        HttpClient::new_with_strategy(HttpClientStrategy::Nulled(stub))
    }
}

#[derive(Default)]
struct HttpClientStub {
    the_only_response: Option<ConfiguredHttpResponse>,
    responses: HashMap<(Url, Method), ConfiguredHttpResponse>,
}

impl HttpClientStub {
    fn with_response(response: ConfiguredHttpResponse) -> Self {
        Self {
            the_only_response: Some(response),
            ..Default::default()
        }
    }

    async fn get_response(&self, method: Method, url: Url) -> anyhow::Result<Response> {
        let response = if let Some(r) = &self.the_only_response {
            r
        } else {
            self.responses
                .get(&(url.clone(), method.clone()))
                .ok_or_else(|| anyhow!("no response configured for {} {}", method, url))?
        };

        match response {
            ConfiguredHttpResponse::Json(i) => Ok(i.clone().into()),
            ConfiguredHttpResponse::Error(e) => Err(anyhow!("{}", e)),
            ConfiguredHttpResponse::Halt => {
                // Yield forever
                loop {
                    poll_fn(|cx| {
                        cx.waker().wake_by_ref();
                        Poll::Pending::<()>
                    }).await;
                }
            },
        }
    }
}

#[derive(Clone)]
pub struct TrackedRequest {
    pub url: Url,
    pub method: Method,
    pub json: serde_json::Value,
}

pub struct Response {
    strategy: ResponseStrategy,
}

impl Response {
    pub fn status(&self) -> StatusCode {
        match &self.strategy {
            ResponseStrategy::Real(resp) => resp.status,
            ResponseStrategy::Nulled(resp) => resp.status,
        }
    }

    pub fn error_for_status(self) -> anyhow::Result<Self> {
        match self.strategy {
            ResponseStrategy::Real(resp) => {
                if resp.status.is_client_error() || resp.status.is_server_error() {
                    Err(anyhow!("HTTP client error ({})", resp.status))
                } else {
                    Ok(Self { strategy: ResponseStrategy::Real(resp) })
                }
            },
            ResponseStrategy::Nulled(resp) => {
                let status = resp.status;
                if status.is_client_error() || status.is_server_error() {
                    Err(anyhow!("Nulled HTTP client error ({})", status))
                } else {
                    Ok(Self {
                        strategy: ResponseStrategy::Nulled(resp),
                    })
                }
            }
        }
    }

    pub async fn json<T: DeserializeOwned>(self) -> anyhow::Result<T> {
        match self.strategy {
            ResponseStrategy::Real(resp) => {
                let deserialized = serde_json::from_str(&resp.body)?;
                Ok(deserialized)
            },
            ResponseStrategy::Nulled(resp) => resp.json(),
        }
    }
}

struct RealResponse {
    status: StatusCode,
    body: String,
}

enum ResponseStrategy {
    Real(RealResponse),
    Nulled(JsonResponse),
}

pub enum ConfiguredHttpResponse {
    Json(JsonResponse),
    Error(String),
    Halt,
}

#[derive(Clone)]
pub struct JsonResponse {
    status: StatusCode,
    body: serde_json::Value,
}

impl JsonResponse {
    pub fn new(status: StatusCode, json: impl Serialize) -> Self {
        Self {
            status,
            body: serde_json::to_value(json).unwrap(),
        }
    }
    pub fn json<T: DeserializeOwned>(&self) -> anyhow::Result<T> {
        let deserialized = serde_json::from_value(self.body.clone())?;
        Ok(deserialized)
    }
}

impl Default for JsonResponse {
    fn default() -> Self {
        Self {
            status: StatusCode::OK,
            body: serde_json::Value::Null,
        }
    }
}

impl From<JsonResponse> for Response {
    fn from(value: JsonResponse) -> Self {
        Self {
            strategy: ResponseStrategy::Nulled(value),
        }
    }
}
