use std::collections::HashMap;

use http::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

/// The message body carried through a Synapse mediation flow.
#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    Empty,
    Json(Value),
    Xml(String),
    Text(String),
    Bytes(Vec<u8>),
}

impl Payload {
    pub fn from_bytes(content_type: Option<&str>, bytes: Vec<u8>) -> Self {
        if bytes.is_empty() {
            return Self::Empty;
        }

        let content_type = content_type.unwrap_or_default().to_ascii_lowercase();
        let looks_json = content_type.contains("json")
            || bytes
                .iter()
                .find(|byte| !byte.is_ascii_whitespace())
                .is_some_and(|byte| *byte == b'{' || *byte == b'[');
        if looks_json {
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                return Self::Json(value);
            }
        }

        let looks_xml = content_type.contains("xml")
            || bytes
                .iter()
                .find(|byte| !byte.is_ascii_whitespace())
                .is_some_and(|byte| *byte == b'<');
        match String::from_utf8(bytes) {
            Ok(text) if looks_xml => Self::Xml(text),
            Ok(text) => Self::Text(text),
            Err(error) => Self::Bytes(error.into_bytes()),
        }
    }

    pub fn text(content: impl Into<String>) -> Self {
        Self::Text(content.into())
    }

    pub fn json(value: Value) -> Self {
        Self::Json(value)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Empty => Vec::new(),
            Self::Json(value) => serde_json::to_vec(value).unwrap_or_default(),
            Self::Xml(text) | Self::Text(text) => text.as_bytes().to_vec(),
            Self::Bytes(bytes) => bytes.clone(),
        }
    }

    pub fn as_text(&self) -> String {
        match self {
            Self::Empty => String::new(),
            Self::Json(value) => serde_json::to_string(value).unwrap_or_default(),
            Self::Xml(text) | Self::Text(text) => text.clone(),
            Self::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        }
    }

    pub fn content_type(&self) -> &'static str {
        match self {
            Self::Json(_) => "application/json",
            Self::Xml(_) => "application/xml",
            Self::Text(_) | Self::Empty | Self::Bytes(_) => "text/plain; charset=utf-8",
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::Json(Value::Null) => true,
            Self::Json(_) => false,
            Self::Xml(text) | Self::Text(text) => text.is_empty(),
            Self::Bytes(bytes) => bytes.is_empty(),
        }
    }
}

/// Synapse property scopes. `default` is the normal message property scope;
/// `axis2` carries transport-independent Axis2 properties such as `HTTP_SC`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PropertyScope {
    Default,
    Axis2,
    Transport,
    Synapse,
    Operation,
}

impl PropertyScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Axis2 => "axis2",
            Self::Transport => "transport",
            Self::Synapse => "synapse",
            Self::Operation => "operation",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "default" | "" => Some(Self::Default),
            "axis2" => Some(Self::Axis2),
            "transport" => Some(Self::Transport),
            "synapse" => Some(Self::Synapse),
            "operation" => Some(Self::Operation),
            _ => None,
        }
    }
}

/// The mutable state a sequence operates on. It intentionally mirrors the parts
/// of `org.apache.synapse.MessageContext` that the supported mediators need.
#[derive(Debug, Clone)]
pub struct MessageContext {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub request_headers: HeaderMap,
    pub response_headers: HeaderMap,
    pub payload: Payload,
    pub http_status: u16,
    properties: HashMap<(PropertyScope, String), String>,
}

impl MessageContext {
    pub fn new(
        method: impl Into<String>,
        path: impl Into<String>,
        query: Option<String>,
        request_headers: HeaderMap,
        payload: Payload,
    ) -> Self {
        Self {
            method: method.into(),
            path: path.into(),
            query,
            request_headers,
            response_headers: HeaderMap::new(),
            payload,
            http_status: 200,
            properties: HashMap::new(),
        }
    }

    pub fn set_property(
        &mut self,
        scope: PropertyScope,
        name: impl Into<String>,
        value: impl Into<String>,
    ) {
        self.properties.insert((scope, name.into()), value.into());
    }

    pub fn get_property(&self, scope: PropertyScope, name: &str) -> Option<&str> {
        self.properties
            .get(&(scope, name.to_owned()))
            .map(String::as_str)
    }

    pub fn remove_property(&mut self, scope: PropertyScope, name: &str) -> Option<String> {
        self.properties.remove(&(scope, name.to_owned()))
    }

    pub fn properties(&self) -> impl Iterator<Item = (&PropertyScope, &String, &String)> {
        self.properties
            .iter()
            .map(|((scope, name), value)| (scope, name, value))
    }

    /// Path after the API context/version, which Synapse exposes as
    /// `REST_URL_POSTFIX`.
    pub fn rest_postfix(&self) -> &str {
        self.get_property(PropertyScope::Default, "REST_URL_POSTFIX")
            .unwrap_or("/")
    }

    pub fn set_header(
        &mut self,
        phase: Phase,
        name: &str,
        value: &str,
    ) -> Result<(), crate::SynapseError> {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            crate::SynapseError::Mediation(format!("invalid header name {name}: {error}"))
        })?;
        let value = HeaderValue::from_str(value).map_err(|error| {
            crate::SynapseError::Mediation(format!("invalid header value: {error}"))
        })?;
        self.headers_mut(phase).insert(name, value);
        Ok(())
    }

    pub fn remove_header(&mut self, phase: Phase, name: &str) {
        if let Ok(name) = HeaderName::from_bytes(name.as_bytes()) {
            self.headers_mut(phase).remove(name);
        }
    }

    pub fn headers(&self, phase: Phase) -> &HeaderMap {
        match phase {
            Phase::In => &self.request_headers,
            Phase::Out | Phase::Fault => &self.response_headers,
        }
    }

    pub fn headers_mut(&mut self, phase: Phase) -> &mut HeaderMap {
        match phase {
            Phase::In => &mut self.request_headers,
            Phase::Out | Phase::Fault => &mut self.response_headers,
        }
    }

    /// `HTTP_SC` is set by fault sequences and by explicit property mediators.
    pub fn effective_status(&self) -> u16 {
        self.get_property(PropertyScope::Axis2, "HTTP_SC")
            .and_then(|value| value.parse().ok())
            .unwrap_or(self.http_status)
    }

    pub fn set_http_status(&mut self, status: u16) {
        self.http_status = status;
        self.set_property(PropertyScope::Axis2, "HTTP_SC", status.to_string());
    }
}

/// Which side of the mediation flow a mediator is currently running on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    In,
    Out,
    Fault,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
            Self::Fault => "fault",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_json_and_xml_payloads() {
        let json = Payload::from_bytes(Some("application/json"), br#"{"a":1}"#.to_vec());
        assert!(matches!(json, Payload::Json(_)));
        let xml = Payload::from_bytes(None, b"<a>1</a>".to_vec());
        assert!(matches!(xml, Payload::Xml(_)));
        let text = Payload::from_bytes(None, b"hello".to_vec());
        assert!(matches!(text, Payload::Text(_)));
        assert!(matches!(
            Payload::from_bytes(None, Vec::new()),
            Payload::Empty
        ));
    }

    #[test]
    fn property_scopes_are_independent() {
        let mut context = MessageContext::new("GET", "/", None, HeaderMap::new(), Payload::Empty);
        context.set_property(PropertyScope::Default, "x", "default");
        context.set_property(PropertyScope::Axis2, "x", "axis2");
        assert_eq!(
            context.get_property(PropertyScope::Default, "x"),
            Some("default")
        );
        assert_eq!(
            context.get_property(PropertyScope::Axis2, "x"),
            Some("axis2")
        );
        assert_eq!(context.effective_status(), 200);
    }
}
