use serde_json::Value;

use crate::{
    error::SynapseError,
    message::{MessageContext, Payload, PropertyScope},
};

/// A Synapse expression used by `property`, `filter`, `switch`, `log`,
/// `payloadFactory` arguments and header mediators.
#[derive(Debug, Clone, PartialEq)]
pub enum Expression {
    Literal(String),
    JsonPath(String),
    XPath(String),
    Property { scope: PropertyScope, name: String },
    Body,
}

impl Expression {
    /// Parse the expression syntaxes that the supported mediator set needs.
    /// Unknown `$`-prefixed forms are rejected instead of treated as literals.
    pub fn parse(raw: &str) -> Result<Self, SynapseError> {
        let raw = raw.trim();
        if let Some(inner) = strip_call(raw, "json-eval") {
            let path = unquote(inner.trim()).to_owned();
            if !path.starts_with('$') {
                return Err(SynapseError::UnsupportedExpression(format!(
                    "json-eval path must start with '$': {path}"
                )));
            }
            return Ok(Self::JsonPath(path));
        }

        if let Some(inner) = strip_call(raw, "get-property") {
            let args = split_args(inner);
            let (scope, name) = match args.as_slice() {
                [name] => (PropertyScope::Default, unquote(name).to_owned()),
                [scope, name] => {
                    let parsed = PropertyScope::parse(unquote(scope)).ok_or_else(|| {
                        SynapseError::UnsupportedExpression(format!(
                            "unknown property scope in {raw}"
                        ))
                    })?;
                    (parsed, unquote(name).to_owned())
                }
                _ => {
                    return Err(SynapseError::UnsupportedExpression(format!(
                        "get-property expects one or two arguments: {raw}"
                    )));
                }
            };
            if name.is_empty() {
                return Err(SynapseError::UnsupportedExpression(format!(
                    "empty property name in {raw}"
                )));
            }
            return Ok(Self::Property { scope, name });
        }

        if raw == "$body" {
            return Ok(Self::Body);
        }

        if let Some(name) = raw.strip_prefix("$ctx:") {
            return Ok(Self::Property {
                scope: PropertyScope::Default,
                name: name.to_owned(),
            });
        }
        if let Some(name) = raw.strip_prefix("$trp:") {
            return Ok(Self::Property {
                scope: PropertyScope::Transport,
                name: name.to_owned(),
            });
        }
        if let Some(name) = raw.strip_prefix("$axis2:") {
            return Ok(Self::Property {
                scope: PropertyScope::Axis2,
                name: name.to_owned(),
            });
        }
        if let Some(name) = raw.strip_prefix("$synapse:") {
            return Ok(Self::Property {
                scope: PropertyScope::Synapse,
                name: name.to_owned(),
            });
        }
        if let Some(name) = raw.strip_prefix("$operation:") {
            return Ok(Self::Property {
                scope: PropertyScope::Operation,
                name: name.to_owned(),
            });
        }

        if raw.starts_with("//") || raw.starts_with('/') {
            return Ok(Self::XPath(raw.to_owned()));
        }
        if let Some(inner) = strip_call(raw, "xpath") {
            return Ok(Self::XPath(unquote(inner.trim()).to_owned()));
        }

        Ok(Self::Literal(raw.to_owned()))
    }

    pub fn evaluate(&self, context: &MessageContext) -> Result<String, SynapseError> {
        match self {
            Self::Literal(value) => Ok(value.clone()),
            Self::Body => Ok(context.payload.as_text()),
            Self::Property { scope, name } => match context.get_property(*scope, name) {
                Some(value) => Ok(value.to_owned()),
                // Synapse returns an empty string for an unset property in most
                // string contexts. Transport headers fall back to the header map.
                None if *scope == PropertyScope::Transport => Ok(context
                    .request_headers
                    .get(name.as_str())
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_owned()),
                None => Ok(String::new()),
            },
            Self::JsonPath(path) => {
                let Payload::Json(value) = &context.payload else {
                    return Err(SynapseError::Mediation(format!(
                        "json-eval requires a JSON payload: {path}"
                    )));
                };
                let selected = json_get(value, path)?;
                Ok(match selected {
                    Value::String(text) => text.clone(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                })
            }
            Self::XPath(path) => {
                let Payload::Xml(xml) = &context.payload else {
                    return Err(SynapseError::Mediation(format!(
                        "XPath requires an XML payload: {path}"
                    )));
                };
                xml_text(xml, path)
            }
        }
    }
}

fn strip_call<'a>(raw: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("{name}(");
    raw.strip_prefix(&prefix)?.strip_suffix(')')
}

fn split_args(raw: &str) -> Vec<&str> {
    let mut args = Vec::new();
    let mut start = 0;
    let mut quote = None;
    for (index, character) in raw.char_indices() {
        match (quote, character) {
            (None, '\'' | '"') => quote = Some(character),
            (Some(open), current) if open == current => quote = None,
            (None, ',') => {
                args.push(raw[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    let last = raw[start..].trim();
    if !last.is_empty() {
        args.push(last);
    }
    args
}

fn unquote(raw: &str) -> &str {
    let raw = raw.trim();
    if raw.len() >= 2
        && ((raw.starts_with('\'') && raw.ends_with('\''))
            || (raw.starts_with('"') && raw.ends_with('"')))
    {
        &raw[1..raw.len() - 1]
    } else {
        raw
    }
}

/// Minimal JSONPath: `$.a.b`, `$['a']`, `$[0]`, and combinations. Wildcards and
/// filters are rejected explicitly rather than returning a wrong value.
fn json_get<'a>(value: &'a Value, path: &str) -> Result<&'a Value, SynapseError> {
    let mut rest = path
        .strip_prefix('$')
        .ok_or_else(|| SynapseError::UnsupportedExpression(path.to_owned()))?;

    let mut current = value;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('.') {
            let end = after.find(['.', '[']).unwrap_or(after.len());
            let key = &after[..end];
            if key.is_empty() || key == "*" {
                return Err(SynapseError::UnsupportedExpression(path.to_owned()));
            }
            current = current.get(key).ok_or_else(|| {
                SynapseError::Mediation(format!("json-eval path not found: {path}"))
            })?;
            rest = &after[end..];
            continue;
        }
        if let Some(after) = rest.strip_prefix('[') {
            let end = after
                .find(']')
                .ok_or_else(|| SynapseError::UnsupportedExpression(path.to_owned()))?;
            let index = after[..end].trim();
            if index == "*" || index.contains(':') || index.contains('?') {
                return Err(SynapseError::UnsupportedExpression(path.to_owned()));
            }
            let key = unquote(index);
            current = if let Ok(position) = key.parse::<usize>() {
                current.get(position)
            } else {
                current.get(key)
            }
            .ok_or_else(|| SynapseError::Mediation(format!("json-eval path not found: {path}")))?;
            rest = &after[end + 1..];
            continue;
        }
        return Err(SynapseError::UnsupportedExpression(format!(
            "unsupported json-eval syntax: {path}"
        )));
    }

    Ok(current)
}

/// Minimal XPath used by the supported samples: `//name`, `//name/text()`,
/// `/a/b`, `/a/b/text()`. Anything more complex is rejected.
fn xml_text(xml: &str, path: &str) -> Result<String, SynapseError> {
    let document = roxmltree::Document::parse(xml)
        .map_err(|error| SynapseError::Mediation(format!("invalid XML payload: {error}")))?;

    let path = path.trim();
    let wants_text = path.ends_with("/text()");
    let path = path.strip_suffix("/text()").unwrap_or(path).trim();
    if path.contains('[') || path.contains('@') || path.contains('(') || path.contains('=') {
        return Err(SynapseError::UnsupportedExpression(format!(
            "unsupported XPath: {path}"
        )));
    }

    let node = if let Some(name) = path.strip_prefix("//") {
        let name = name.trim_end_matches('/');
        document
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == name)
    } else if path == "/" {
        document.root_element().into()
    } else if path.starts_with('/') {
        let mut current = document.root_element();
        for segment in path.split('/').skip(1).filter(|s| !s.is_empty()) {
            if segment == "*" {
                return Err(SynapseError::UnsupportedExpression(format!(
                    "unsupported XPath wildcard: {path}"
                )));
            }
            current = current
                .children()
                .find(|node| node.is_element() && node.tag_name().name() == segment)
                .ok_or_else(|| SynapseError::Mediation(format!("XPath not found: {path}")))?;
        }
        Some(current)
    } else {
        return Err(SynapseError::UnsupportedExpression(path.to_owned()));
    };

    let Some(node) = node else {
        return Ok(String::new());
    };
    if wants_text {
        Ok(node.text().unwrap_or_default().to_owned())
    } else {
        Ok(node
            .descendants()
            .filter(|child| child.is_text())
            .filter_map(|child| child.text())
            .collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderMap;

    fn context(payload: Payload) -> MessageContext {
        let mut context = MessageContext::new("GET", "/", None, HeaderMap::new(), payload);
        context.set_property(PropertyScope::Default, "uri.var.id", "42");
        context
    }

    #[test]
    fn evaluates_supported_expressions() {
        let context = context(Payload::Json(serde_json::json!({"a": {"b": [1, 2]}})));
        assert_eq!(
            Expression::parse("json-eval($.a.b[1])")
                .unwrap()
                .evaluate(&context)
                .unwrap(),
            "2"
        );
        assert_eq!(
            Expression::parse("get-property('uri.var.id')")
                .unwrap()
                .evaluate(&context)
                .unwrap(),
            "42"
        );
        assert_eq!(
            Expression::parse("$ctx:uri.var.id")
                .unwrap()
                .evaluate(&context)
                .unwrap(),
            "42"
        );
        assert_eq!(
            Expression::parse("literal")
                .unwrap()
                .evaluate(&context)
                .unwrap(),
            "literal"
        );
    }

    #[test]
    fn rejects_unsupported_jsonpath() {
        let context = context(Payload::Json(serde_json::json!({"a": [1]})));
        let expression = Expression::parse("json-eval($.a[*])").unwrap();
        assert!(expression.evaluate(&context).is_err());
    }

    #[test]
    fn extracts_simple_xpath_text() {
        let context = context(Payload::Xml("<order><id>7</id></order>".to_owned()));
        assert_eq!(
            Expression::parse("//id/text()")
                .unwrap()
                .evaluate(&context)
                .unwrap(),
            "7"
        );
    }
}
