use roxmltree::Node;

use crate::{
    error::SynapseError,
    sequence::{parse_optional_endpoint, parse_sequence, required_attr, Sequence},
};

/// A Synapse `<api>` artifact.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiArtifact {
    pub name: String,
    pub context: String,
    pub version: Option<String>,
    /// API-level backend endpoint. Synapse `<api>` artifacts normally select an
    /// endpoint inside `<call>/<send>`; APIM definitions set this explicitly.
    pub endpoint: Option<String>,
    pub in_sequence: Option<Sequence>,
    pub out_sequence: Option<Sequence>,
    pub fault_sequence: Option<Sequence>,
    pub resources: Vec<ResourceArtifact>,
}

impl ApiArtifact {
    /// External base path, e.g. `/pizzashack/1.0.0`.
    pub fn full_path(&self) -> String {
        let context = self.context.trim_end_matches('/');
        match self
            .version
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            Some(version) if !context.ends_with(&format!("/{version}")) => {
                format!("{context}/{version}")
            }
            _ => context.to_owned(),
        }
    }
}

/// One `<resource>` inside a Synapse API or one operation in an APIM
/// definition. The same shape is used so the gateway has a single conversion
/// path.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceArtifact {
    pub uri_template: String,
    pub methods: Vec<String>,
    pub in_sequence: Option<Sequence>,
    pub out_sequence: Option<Sequence>,
    pub fault_sequence: Option<Sequence>,
}

/// A named Synapse endpoint definition (`<endpoint name="...">`).
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointDefinition {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Default)]
pub struct SynapseDocument {
    pub apis: Vec<ApiArtifact>,
    pub sequences: Vec<Sequence>,
    pub endpoints: Vec<EndpointDefinition>,
    /// Root elements that are valid Synapse XML but outside the supported
    /// subset (proxy services, message stores, tasks, ...). The bundle keeps
    /// these visible so operators can see what was not loaded.
    pub ignored: Vec<String>,
}

/// Parse a standalone Synapse artifact, failing if it contains no supported
/// root element.
pub fn parse(xml: &str) -> Result<SynapseDocument, SynapseError> {
    let document = parse_document(xml)?;
    if document.apis.is_empty() && document.sequences.is_empty() && document.endpoints.is_empty() {
        return Err(SynapseError::UnsupportedArtifact(
            document
                .ignored
                .first()
                .cloned()
                .unwrap_or_else(|| "empty document".to_owned()),
        ));
    }
    Ok(document)
}

/// Parse any Synapse XML and return the artifacts that belong to the supported
/// subset. Unknown roots are recorded in `ignored` instead of failing, because a
/// real CAR contains proxy services, tasks and other artifact types that the
/// gateway does not execute yet.
pub fn parse_document(xml: &str) -> Result<SynapseDocument, SynapseError> {
    let document =
        roxmltree::Document::parse(xml).map_err(|error| SynapseError::Xml(error.to_string()))?;
    let root = document.root_element();
    let mut result = SynapseDocument::default();

    match root.tag_name().name() {
        "api" => result.apis.push(parse_api(root)?),
        "sequence" => result.sequences.push(parse_named_sequence(root)?),
        "endpoint" => result.endpoints.push(parse_endpoint_definition(root)?),
        "definitions" => {
            for child in root.children().filter(Node::is_element) {
                match child.tag_name().name() {
                    "api" => result.apis.push(parse_api(child)?),
                    "sequence" => result.sequences.push(parse_named_sequence(child)?),
                    "endpoint" => result.endpoints.push(parse_endpoint_definition(child)?),
                    "definitions" => {}
                    other => result.ignored.push(other.to_owned()),
                }
            }
        }
        other => result.ignored.push(other.to_owned()),
    }

    Ok(result)
}

fn parse_api(node: Node) -> Result<ApiArtifact, SynapseError> {
    let name = required_attr(node, "name")?.to_owned();
    let context = required_attr(node, "context")?.to_owned();
    if !context.starts_with('/') {
        return Err(SynapseError::xml(format!(
            "API {name} context must start with '/': {context}"
        )));
    }

    let mut in_sequence = None;
    let mut out_sequence = None;
    let mut fault_sequence = None;
    let mut resources = Vec::new();

    for child in node.children().filter(Node::is_element) {
        match child.tag_name().name() {
            "resource" => resources.push(parse_resource(child)?),
            "inSequence" => in_sequence = Some(parse_sequence(child)?),
            "outSequence" => out_sequence = Some(parse_sequence(child)?),
            "faultSequence" => fault_sequence = Some(parse_sequence(child)?),
            other => {
                return Err(SynapseError::UnsupportedArtifact(format!(
                    "{other} in API {name}"
                )));
            }
        }
    }

    if resources.is_empty() {
        return Err(SynapseError::xml(format!("API {name} has no resources")));
    }

    Ok(ApiArtifact {
        name,
        context,
        version: node.attribute("version").map(ToOwned::to_owned),
        endpoint: None,
        in_sequence,
        out_sequence,
        fault_sequence,
        resources,
    })
}

fn parse_resource(node: Node) -> Result<ResourceArtifact, SynapseError> {
    let uri_template = required_attr(node, "uri-template")?.to_owned();
    if !uri_template.starts_with('/') {
        return Err(SynapseError::xml(format!(
            "resource uri-template must start with '/': {uri_template}"
        )));
    }
    let methods = required_attr(node, "methods")?
        .split(|character: char| character.is_whitespace() || character == ',')
        .filter(|method| !method.is_empty())
        .map(|method| method.to_ascii_uppercase())
        .collect::<Vec<_>>();
    if methods.is_empty() {
        return Err(SynapseError::xml(format!(
            "resource {uri_template} has no methods"
        )));
    }

    let mut in_sequence = None;
    let mut out_sequence = None;
    let mut fault_sequence = None;
    for child in node.children().filter(Node::is_element) {
        match child.tag_name().name() {
            "inSequence" => in_sequence = Some(parse_sequence(child)?),
            "outSequence" => out_sequence = Some(parse_sequence(child)?),
            "faultSequence" => fault_sequence = Some(parse_sequence(child)?),
            other => {
                return Err(SynapseError::UnsupportedArtifact(format!(
                    "{other} in resource {uri_template}"
                )));
            }
        }
    }

    Ok(ResourceArtifact {
        uri_template,
        methods,
        in_sequence,
        out_sequence,
        fault_sequence,
    })
}

fn parse_named_sequence(node: Node) -> Result<Sequence, SynapseError> {
    let name = required_attr(node, "name")?.to_owned();
    let mut sequence = parse_sequence(node)?;
    sequence.name = Some(name);
    Ok(sequence)
}

fn parse_endpoint_definition(node: Node) -> Result<EndpointDefinition, SynapseError> {
    let name = required_attr(node, "name")?.to_owned();
    let endpoint = parse_optional_endpoint(node)?.ok_or_else(|| {
        SynapseError::UnsupportedEndpoint(format!("endpoint {name} has no address"))
    })?;
    let url = match endpoint {
        crate::sequence::EndpointRef::Inline(url) => url,
        crate::sequence::EndpointRef::Named(other) => {
            return Err(SynapseError::UnsupportedEndpoint(format!(
                "endpoint {name} aliases another endpoint ({other}); aliases are not supported"
            )));
        }
    };
    Ok(EndpointDefinition { name, url })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sequence::Mediator;

    #[test]
    fn parses_api_with_resources_and_sequences() {
        let document = parse(
            r#"
            <api xmlns="http://ws.apache.org/ns/synapse"
                 name="PizzaShackAPI" context="/pizzashack" version="1.0.0">
              <resource methods="GET POST" uri-template="/menu/{id}">
                <inSequence>
                  <property name="id" expression="get-property('uri.var.id')"/>
                  <respond/>
                </inSequence>
                <outSequence><send/></outSequence>
                <faultSequence><respond/></faultSequence>
              </resource>
            </api>
            "#,
        )
        .expect("api");

        let api = &document.apis[0];
        assert_eq!(api.name, "PizzaShackAPI");
        assert_eq!(api.full_path(), "/pizzashack/1.0.0");
        assert_eq!(api.resources[0].methods, vec!["GET", "POST"]);
        assert_eq!(api.resources[0].uri_template, "/menu/{id}");
        assert!(matches!(
            api.resources[0].in_sequence.as_ref().unwrap().mediators[1],
            Mediator::Respond
        ));
    }

    #[test]
    fn parses_named_sequence_and_endpoint() {
        let document = parse(
            r#"
            <definitions xmlns="http://ws.apache.org/ns/synapse">
              <sequence name="common"><log/><respond/></sequence>
              <endpoint name="backend"><address uri="http://backend:8080"/></endpoint>
            </definitions>
            "#,
        )
        .expect("definitions");
        assert_eq!(document.sequences[0].name.as_deref(), Some("common"));
        assert_eq!(document.endpoints[0].url, "http://backend:8080");
    }
}
