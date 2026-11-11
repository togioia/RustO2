use roxmltree::Node;

use crate::{error::SynapseError, expression::Expression, message::PropertyScope};

/// A Synapse sequence. `name` is set for named sequences and for `key`
/// references; the engine resolves references through the plan's sequence map.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sequence {
    pub name: Option<String>,
    pub mediators: Vec<Mediator>,
}

impl Sequence {
    pub fn new(mediators: Vec<Mediator>) -> Self {
        Self {
            name: None,
            mediators,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.mediators.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mediator {
    Log(LogMediator),
    Property(PropertyMediator),
    Filter(FilterMediator),
    Switch(SwitchMediator),
    PayloadFactory(PayloadFactoryMediator),
    Enrich(EnrichMediator),
    Call(CallMediator),
    Send(SendMediator),
    Respond,
    Drop,
    Header(HeaderMediator),
    SequenceRef(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Simple,
    Custom,
    Full,
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    fn parse(value: Option<&str>) -> Result<Self, SynapseError> {
        match value
            .unwrap_or("simple")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "simple" => Ok(Self::Simple),
            "custom" => Ok(Self::Custom),
            "full" => Ok(Self::Full),
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            other => Err(SynapseError::xml(format!("unknown log level '{other}'"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogProperty {
    pub name: Option<String>,
    pub value: Option<String>,
    pub expression: Option<Expression>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogMediator {
    pub level: LogLevel,
    pub category: Option<String>,
    pub separator: Option<String>,
    pub properties: Vec<LogProperty>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyAction {
    Set,
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyType {
    String,
    Integer,
    Boolean,
    Double,
    Float,
    Long,
    Short,
    Om,
    Json,
}

impl PropertyType {
    fn parse(value: Option<&str>) -> Result<Self, SynapseError> {
        match value
            .unwrap_or("STRING")
            .trim()
            .to_ascii_uppercase()
            .as_str()
        {
            "STRING" => Ok(Self::String),
            "INTEGER" => Ok(Self::Integer),
            "BOOLEAN" => Ok(Self::Boolean),
            "DOUBLE" => Ok(Self::Double),
            "FLOAT" => Ok(Self::Float),
            "LONG" => Ok(Self::Long),
            "SHORT" => Ok(Self::Short),
            "OM" => Ok(Self::Om),
            "JSON" => Ok(Self::Json),
            other => Err(SynapseError::xml(format!(
                "unsupported property type '{other}'"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PropertyMediator {
    pub name: String,
    pub value: Option<String>,
    pub expression: Option<Expression>,
    pub scope: PropertyScope,
    pub action: PropertyAction,
    pub data_type: PropertyType,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FilterMediator {
    pub source: Option<Expression>,
    pub regex: Option<String>,
    pub xpath: Option<String>,
    pub then: Sequence,
    pub otherwise: Option<Sequence>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SwitchCase {
    pub regex: Option<String>,
    pub value: Option<String>,
    pub sequence: Sequence,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SwitchMediator {
    pub source: Expression,
    pub cases: Vec<SwitchCase>,
    pub default: Option<Sequence>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PayloadArg {
    pub expression: Option<Expression>,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PayloadFactoryMediator {
    pub media_type: String,
    pub format: String,
    pub args: Vec<PayloadArg>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrichTargetType {
    Body,
    Envelope,
    Property,
    Header,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrichSourceType {
    Inline,
    Body,
    Envelope,
    Property,
    Header,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrichAction {
    Replace,
    Append,
    Prepend,
    Child,
    Remove,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnrichMediator {
    pub target_type: EnrichTargetType,
    pub target_action: EnrichAction,
    pub target_name: Option<String>,
    pub source_type: EnrichSourceType,
    pub source_name: Option<String>,
    pub source_inline: Option<String>,
    pub source_media_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EndpointRef {
    Inline(String),
    Named(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallMediator {
    pub endpoint: Option<EndpointRef>,
    pub blocking: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SendMediator {
    pub endpoint: Option<EndpointRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderAction {
    Set,
    Remove,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeaderMediator {
    pub name: String,
    pub value: Option<String>,
    pub expression: Option<Expression>,
    pub action: HeaderAction,
    pub scope: HeaderScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderScope {
    Transport,
    Default,
}

pub(crate) fn parse_mediators(parent: Node) -> Result<Vec<Mediator>, SynapseError> {
    parent
        .children()
        .filter(Node::is_element)
        .map(parse_mediator)
        .collect()
}

pub(crate) fn parse_mediator(node: Node) -> Result<Mediator, SynapseError> {
    let name = node.tag_name().name();
    match name {
        "log" => parse_log(node),
        "property" => parse_property(node),
        "filter" => parse_filter(node),
        "switch" => parse_switch(node),
        "payloadFactory" => parse_payload_factory(node),
        "enrich" => parse_enrich(node),
        "call" => parse_call(node),
        "send" => parse_send(node),
        "respond" => Ok(Mediator::Respond),
        "drop" => Ok(Mediator::Drop),
        "header" => parse_header(node),
        "sequence" => parse_sequence_reference(node),
        other => Err(SynapseError::UnsupportedMediator {
            name: other.to_owned(),
            location: location(node),
        }),
    }
}

/// Parse a sequence wrapper (`<inSequence>`, `<outSequence>`, `<faultSequence>`,
/// `<then>`, `<else>`, `<case>`, `<default>`) or a standalone `<sequence>`.
pub(crate) fn parse_sequence(node: Node) -> Result<Sequence, SynapseError> {
    if let Some(key) = node.attribute("key") {
        return Ok(Sequence {
            name: Some(key.to_owned()),
            mediators: vec![Mediator::SequenceRef(key.to_owned())],
        });
    }

    // A nested <sequence> wrapper is common inside <then>/<else>.
    let mediators = if node.tag_name().name() != "sequence" {
        if let Some(wrapper) = node
            .children()
            .find(|child| child.is_element() && child.tag_name().name() == "sequence")
        {
            return parse_sequence(wrapper);
        }
        parse_mediators(node)?
    } else {
        parse_mediators(node)?
    };

    Ok(Sequence {
        name: None,
        mediators,
    })
}

fn parse_log(node: Node) -> Result<Mediator, SynapseError> {
    let level = LogLevel::parse(node.attribute("level"))?;
    let properties = node
        .children()
        .filter(|child| child.is_element() && child.tag_name().name() == "property")
        .map(|child| {
            Ok(LogProperty {
                name: child.attribute("name").map(ToOwned::to_owned),
                value: child.attribute("value").map(ToOwned::to_owned),
                expression: child
                    .attribute("expression")
                    .map(Expression::parse)
                    .transpose()?,
            })
        })
        .collect::<Result<Vec<_>, SynapseError>>()?;

    Ok(Mediator::Log(LogMediator {
        level,
        category: node.attribute("category").map(ToOwned::to_owned),
        separator: node.attribute("separator").map(ToOwned::to_owned),
        properties,
    }))
}

fn parse_property(node: Node) -> Result<Mediator, SynapseError> {
    let name = required_attr(node, "name")?.to_owned();
    let action = match node.attribute("action").unwrap_or("set").trim() {
        "set" => PropertyAction::Set,
        "remove" => PropertyAction::Remove,
        other => {
            return Err(SynapseError::xml(format!(
                "unsupported property action '{other}' at {}",
                location(node)
            )));
        }
    };
    let scope = node
        .attribute("scope")
        .and_then(PropertyScope::parse)
        .unwrap_or(PropertyScope::Default);
    let data_type = PropertyType::parse(node.attribute("type"))?;

    Ok(Mediator::Property(PropertyMediator {
        name,
        value: node.attribute("value").map(ToOwned::to_owned),
        expression: node
            .attribute("expression")
            .map(Expression::parse)
            .transpose()?,
        scope,
        action,
        data_type,
    }))
}

fn parse_filter(node: Node) -> Result<Mediator, SynapseError> {
    let source = node
        .attribute("source")
        .map(Expression::parse)
        .transpose()?;
    let regex = node.attribute("regex").map(ToOwned::to_owned);
    let xpath = node.attribute("xpath").map(ToOwned::to_owned);
    if regex.is_none() && xpath.is_none() {
        return Err(SynapseError::xml(format!(
            "filter at {} requires regex or xpath",
            location(node)
        )));
    }

    let then = child_sequence(node, "then")?.unwrap_or_default();
    let otherwise = child_sequence(node, "else")?;

    Ok(Mediator::Filter(FilterMediator {
        source,
        regex,
        xpath,
        then,
        otherwise,
    }))
}

fn parse_switch(node: Node) -> Result<Mediator, SynapseError> {
    let source = Expression::parse(required_attr(node, "source")?)?;
    let mut cases = Vec::new();
    let mut default = None;

    for child in node.children().filter(Node::is_element) {
        match child.tag_name().name() {
            "case" => cases.push(SwitchCase {
                regex: child.attribute("regex").map(ToOwned::to_owned),
                value: child.attribute("value").map(ToOwned::to_owned),
                sequence: parse_sequence(child)?,
            }),
            "default" => default = Some(parse_sequence(child)?),
            other => {
                return Err(SynapseError::UnsupportedMediator {
                    name: other.to_owned(),
                    location: location(child),
                });
            }
        }
    }

    if cases.is_empty() {
        return Err(SynapseError::xml(format!(
            "switch at {} has no case elements",
            location(node)
        )));
    }

    Ok(Mediator::Switch(SwitchMediator {
        source,
        cases,
        default,
    }))
}

fn parse_payload_factory(node: Node) -> Result<Mediator, SynapseError> {
    let media_type = node
        .attribute("media-type")
        .unwrap_or("application/xml")
        .to_owned();
    let format = node
        .children()
        .find(|child| child.is_element() && child.tag_name().name() == "format")
        .map(inner_content)
        .ok_or_else(|| {
            SynapseError::xml(format!(
                "payloadFactory at {} has no <format>",
                location(node)
            ))
        })?;

    let mut args = Vec::new();
    if let Some(args_node) = node
        .children()
        .find(|child| child.is_element() && child.tag_name().name() == "args")
    {
        for arg in args_node
            .children()
            .filter(|child| child.is_element() && child.tag_name().name() == "arg")
        {
            if let Some(expression) = arg.attribute("expression") {
                args.push(PayloadArg {
                    expression: Some(Expression::parse(expression)?),
                    value: None,
                });
            } else if let Some(value) = arg.attribute("value") {
                args.push(PayloadArg {
                    expression: None,
                    value: Some(value.to_owned()),
                });
            } else {
                args.push(PayloadArg {
                    expression: None,
                    value: Some(inner_content(arg)),
                });
            }
        }
    }

    Ok(Mediator::PayloadFactory(PayloadFactoryMediator {
        media_type,
        format,
        args,
    }))
}

fn parse_enrich(node: Node) -> Result<Mediator, SynapseError> {
    let target = node
        .children()
        .find(|child| child.is_element() && child.tag_name().name() == "target")
        .ok_or_else(|| {
            SynapseError::xml(format!("enrich at {} has no <target>", location(node)))
        })?;
    let source = node
        .children()
        .find(|child| child.is_element() && child.tag_name().name() == "source")
        .ok_or_else(|| {
            SynapseError::xml(format!("enrich at {} has no <source>", location(node)))
        })?;

    let target_type = match target.attribute("type").unwrap_or("body") {
        "body" => EnrichTargetType::Body,
        "envelope" => EnrichTargetType::Envelope,
        "property" => EnrichTargetType::Property,
        "header" => EnrichTargetType::Header,
        other => {
            return Err(SynapseError::UnsupportedEndpoint(format!(
                "enrich target type '{other}' at {}",
                location(target)
            )));
        }
    };
    let target_action = match target
        .attribute("action")
        .or_else(|| source.attribute("action"))
        .unwrap_or("replace")
    {
        "replace" => EnrichAction::Replace,
        "append" => EnrichAction::Append,
        "prepend" => EnrichAction::Prepend,
        "child" => EnrichAction::Child,
        "remove" => EnrichAction::Remove,
        other => {
            return Err(SynapseError::xml(format!(
                "unsupported enrich action '{other}' at {}",
                location(target)
            )));
        }
    };
    let source_type = match source.attribute("type").unwrap_or("inline") {
        "inline" => EnrichSourceType::Inline,
        "body" => EnrichSourceType::Body,
        "envelope" => EnrichSourceType::Envelope,
        "property" => EnrichSourceType::Property,
        "header" => EnrichSourceType::Header,
        other => {
            return Err(SynapseError::UnsupportedEndpoint(format!(
                "enrich source type '{other}' at {}",
                location(source)
            )));
        }
    };

    Ok(Mediator::Enrich(EnrichMediator {
        target_type,
        target_action,
        target_name: target
            .attribute("property")
            .or_else(|| target.attribute("name"))
            .map(ToOwned::to_owned),
        source_type,
        source_name: source
            .attribute("property")
            .or_else(|| source.attribute("name"))
            .map(ToOwned::to_owned),
        source_inline: (source_type == EnrichSourceType::Inline).then(|| inner_content(source)),
        source_media_type: source.attribute("media-type").map(ToOwned::to_owned),
    }))
}

fn parse_call(node: Node) -> Result<Mediator, SynapseError> {
    let blocking = node
        .attribute("blocking")
        .map(|value| value.eq_ignore_ascii_case("true"))
        .unwrap_or(true);
    let endpoint = node
        .children()
        .find(|child| child.is_element() && child.tag_name().name() == "endpoint")
        .map(parse_optional_endpoint)
        .transpose()?
        .flatten();
    Ok(Mediator::Call(CallMediator { endpoint, blocking }))
}

fn parse_send(node: Node) -> Result<Mediator, SynapseError> {
    let endpoint = node
        .children()
        .find(|child| child.is_element() && child.tag_name().name() == "endpoint")
        .map(parse_optional_endpoint)
        .transpose()?
        .flatten();
    Ok(Mediator::Send(SendMediator { endpoint }))
}

fn parse_header(node: Node) -> Result<Mediator, SynapseError> {
    let name = required_attr(node, "name")?.to_owned();
    let action = match node.attribute("action").unwrap_or("set").trim() {
        "set" => HeaderAction::Set,
        "remove" => HeaderAction::Remove,
        other => {
            return Err(SynapseError::xml(format!(
                "unsupported header action '{other}' at {}",
                location(node)
            )));
        }
    };
    let scope = match node.attribute("scope").unwrap_or("transport") {
        "transport" => HeaderScope::Transport,
        "default" => HeaderScope::Default,
        other => {
            return Err(SynapseError::xml(format!(
                "unsupported header scope '{other}' at {}",
                location(node)
            )));
        }
    };

    Ok(Mediator::Header(HeaderMediator {
        name,
        value: node.attribute("value").map(ToOwned::to_owned),
        expression: node
            .attribute("expression")
            .map(Expression::parse)
            .transpose()?,
        action,
        scope,
    }))
}

fn parse_sequence_reference(node: Node) -> Result<Mediator, SynapseError> {
    let key = required_attr(node, "key")?;
    Ok(Mediator::SequenceRef(key.to_owned()))
}

pub(crate) fn parse_optional_endpoint(node: Node) -> Result<Option<EndpointRef>, SynapseError> {
    if let Some(key) = node.attribute("key") {
        return Ok(Some(EndpointRef::Named(key.to_owned())));
    }
    if let Some(uri) = node.attribute("uri").or_else(|| node.attribute("url")) {
        return Ok(Some(EndpointRef::Inline(uri.to_owned())));
    }

    for child in node.children().filter(Node::is_element) {
        match child.tag_name().name() {
            "address" | "http" | "https" => {
                let uri = child
                    .attribute("uri")
                    .or_else(|| child.attribute("url"))
                    .ok_or_else(|| {
                        SynapseError::UnsupportedEndpoint(format!(
                            "endpoint at {} has no URI",
                            location(child)
                        ))
                    })?;
                return Ok(Some(EndpointRef::Inline(uri.to_owned())));
            }
            "default" => {
                if let Some(endpoint) = parse_optional_endpoint(child)? {
                    return Ok(Some(endpoint));
                }
            }
            "wsdl" => {
                return Err(SynapseError::UnsupportedEndpoint(format!(
                    "WSDL endpoints are not supported at {}",
                    location(child)
                )));
            }
            other => {
                return Err(SynapseError::UnsupportedEndpoint(format!(
                    "endpoint element '{other}' at {}",
                    location(child)
                )));
            }
        }
    }

    // An empty <endpoint/> means "use the route's default endpoint".
    Ok(None)
}

fn child_sequence(parent: Node, name: &str) -> Result<Option<Sequence>, SynapseError> {
    parent
        .children()
        .find(|child| child.is_element() && child.tag_name().name() == name)
        .map(parse_sequence)
        .transpose()
}

pub(crate) fn required_attr<'a>(node: Node<'a, 'a>, name: &str) -> Result<&'a str, SynapseError> {
    node.attribute(name).ok_or_else(|| {
        SynapseError::xml(format!(
            "<{}> at {} is missing required attribute '{name}'",
            node.tag_name().name(),
            location(node)
        ))
    })
}

pub(crate) fn location(node: Node) -> String {
    let position = node.document().text_pos_at(node.range().start);
    format!(
        "{}:{} (line {}, column {})",
        node.tag_name().name(),
        node.range().start,
        position.row,
        position.col
    )
}

/// Raw inner content of a node. Used for `<format>` and inline enrich sources,
/// which can contain JSON text, XML children or CDATA.
fn inner_content(node: Node) -> String {
    // Text-only content (the common case for <format> and inline enrich) must
    // be entity-decoded and CDATA-unwrapped. roxmltree already exposes CDATA as
    // text, so collecting text nodes gives the right result.
    if node.children().all(|child| !child.is_element()) {
        return node
            .descendants()
            .filter(|child| child.is_text())
            .filter_map(|child| child.text())
            .collect();
    }

    // Inline XML sources keep their child elements verbatim.
    let document = node.document().input_text();
    let raw = &document[node.range()];
    let start = raw.find('>').map(|index| index + 1).unwrap_or(0);
    let end = raw.rfind('<').unwrap_or(raw.len());
    if start >= end {
        String::new()
    } else {
        raw[start..end].to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(xml: &str) -> Result<Vec<Mediator>, SynapseError> {
        let document = roxmltree::Document::parse(xml).expect("xml");
        parse_mediators(document.root_element())
    }

    #[test]
    fn parses_core_mediators() {
        let mediators = parse(
            r#"
            <sequence xmlns="http://ws.apache.org/ns/synapse">
              <log level="custom"><property name="id" expression="get-property('uri.var.id')"/></log>
              <property name="seen" value="yes" scope="default"/>
              <filter source="get-property('seen')" regex="yes">
                <then><property name="branch" value="then"/></then>
                <else><property name="branch" value="else"/></else>
              </filter>
              <switch source="get-property('branch')">
                <case regex="then"><property name="result" value="ok"/></case>
                <default><property name="result" value="no"/></default>
              </switch>
              <payloadFactory media-type="json">
                <format>{"id":"$1"}</format>
                <args><arg expression="get-property('uri.var.id')"/></args>
              </payloadFactory>
              <header name="X-Test" value="1" scope="transport"/>
              <respond/>
            </sequence>
            "#,
        )
        .expect("parsed");

        assert_eq!(mediators.len(), 7);
        assert!(matches!(mediators[0], Mediator::Log(_)));
        assert!(matches!(mediators[4], Mediator::PayloadFactory(_)));
        assert!(matches!(mediators[6], Mediator::Respond));
    }

    #[test]
    fn rejects_unknown_mediator_explicitly() {
        let error = parse(
            r#"<sequence xmlns="http://ws.apache.org/ns/synapse">
                 <script language="js">x</script>
               </sequence>"#,
        )
        .expect_err("must reject");
        match error {
            SynapseError::UnsupportedMediator { name, .. } => assert_eq!(name, "script"),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn parses_enrich_and_call() {
        let mediators = parse(
            r#"
            <sequence xmlns="http://ws.apache.org/ns/synapse">
              <enrich>
                <target type="body"/>
                <source type="inline" media-type="application/json">{"a":1}</source>
              </enrich>
              <call blocking="true">
                <endpoint><address uri="http://backend:8080"/></endpoint>
              </call>
              <respond/>
            </sequence>
            "#,
        )
        .expect("parsed");

        let Mediator::Enrich(enrich) = &mediators[0] else {
            panic!("expected enrich");
        };
        assert_eq!(enrich.source_type, EnrichSourceType::Inline);
        assert_eq!(enrich.source_inline.as_deref(), Some(r#"{"a":1}"#));
        let Mediator::Call(call) = &mediators[1] else {
            panic!("expected call");
        };
        assert_eq!(
            call.endpoint,
            Some(EndpointRef::Inline("http://backend:8080".to_owned()))
        );
    }
}
