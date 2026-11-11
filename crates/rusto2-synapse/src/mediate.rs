use std::{collections::HashMap, future::Future, pin::Pin};

use http::{
    header::{CONTENT_TYPE, HOST},
    HeaderMap, HeaderName, HeaderValue, Method,
};
use regex::Regex;
use tracing::Level;

use crate::{
    apim::{ApiDefinition, OperationDefinition},
    bundle::ArtifactBundle,
    error::SynapseError,
    expression::Expression,
    message::{MessageContext, Payload, Phase, PropertyScope},
    sequence::{
        CallMediator, EndpointRef, EnrichAction, EnrichMediator, EnrichSourceType,
        EnrichTargetType, FilterMediator, HeaderAction, HeaderMediator, LogLevel, LogMediator,
        Mediator, PayloadFactoryMediator, PropertyAction, PropertyMediator, PropertyType,
        SendMediator, Sequence, SwitchMediator,
    },
    synapse::{ApiArtifact, ResourceArtifact},
    transport::{Transport, UpstreamRequest},
};

const MAX_SEQUENCE_DEPTH: usize = 32;

/// Everything needed to execute one WSO2 resource: the in/out/fault sequences,
/// default endpoint and named definitions they reference.
#[derive(Debug, Clone)]
pub struct MediationPlan {
    pub name: String,
    /// API base path, e.g. `/pizzashack/1.0.0`.
    pub api_path: String,
    pub default_upstream: Option<String>,
    pub in_sequence: Sequence,
    pub out_sequence: Sequence,
    pub fault_sequence: Sequence,
    pub named_sequences: HashMap<String, Sequence>,
    pub endpoints: HashMap<String, String>,
}

impl MediationPlan {
    pub fn from_api(
        api: &ApiArtifact,
        resource: &ResourceArtifact,
        bundle: &ArtifactBundle,
        fallback_upstream: Option<&str>,
    ) -> Result<Self, SynapseError> {
        let plan = Self {
            name: format!("{} {}", api.name, resource.uri_template),
            api_path: api.full_path(),
            default_upstream: api
                .endpoint
                .clone()
                .or_else(|| fallback_upstream.map(ToOwned::to_owned)),
            in_sequence: resource
                .in_sequence
                .clone()
                .or_else(|| api.in_sequence.clone())
                .unwrap_or_else(default_in_sequence),
            out_sequence: resource
                .out_sequence
                .clone()
                .or_else(|| api.out_sequence.clone())
                .unwrap_or_else(default_out_sequence),
            fault_sequence: resource
                .fault_sequence
                .clone()
                .or_else(|| api.fault_sequence.clone())
                .unwrap_or_default(),
            named_sequences: bundle.sequences.clone(),
            endpoints: bundle.endpoints.clone(),
        };
        plan.validate()?;
        Ok(plan)
    }

    pub fn from_definition(
        api: &ApiDefinition,
        operation: &OperationDefinition,
    ) -> Result<Self, SynapseError> {
        let plan = Self {
            name: format!("{} {} {}", api.name, operation.verb, operation.target),
            api_path: api.full_path(),
            default_upstream: Some(api.endpoint.clone()),
            in_sequence: default_in_sequence(),
            out_sequence: default_out_sequence(),
            fault_sequence: Sequence::default(),
            named_sequences: HashMap::new(),
            endpoints: HashMap::new(),
        };
        plan.validate()?;
        Ok(plan)
    }

    pub fn validate(&self) -> Result<(), SynapseError> {
        if self.name.trim().is_empty() {
            return Err(SynapseError::Mediation("plan has no name".to_owned()));
        }
        if !self.api_path.starts_with('/') {
            return Err(SynapseError::Mediation(format!(
                "API path must start with '/': {}",
                self.api_path
            )));
        }
        if let Some(upstream) = &self.default_upstream {
            validate_http_url(upstream)?;
        }
        validate_sequence(self, &self.in_sequence, 0)?;
        validate_sequence(self, &self.out_sequence, 0)?;
        validate_sequence(self, &self.fault_sequence, 0)?;
        Ok(())
    }

    fn resolve_endpoint(&self, endpoint: Option<&EndpointRef>) -> Result<String, SynapseError> {
        match endpoint {
            Some(EndpointRef::Inline(url)) => Ok(url.clone()),
            Some(EndpointRef::Named(name)) => self
                .endpoints
                .get(name)
                .cloned()
                .ok_or_else(|| SynapseError::UnknownEndpoint(name.clone())),
            None => self.default_upstream.clone().ok_or_else(|| {
                SynapseError::Mediation(format!(
                    "{} has no endpoint and the API declares no default endpoint",
                    self.name
                ))
            }),
        }
    }
}

fn default_in_sequence() -> Sequence {
    Sequence::new(vec![Mediator::Call(CallMediator {
        endpoint: None,
        blocking: true,
    })])
}

fn default_out_sequence() -> Sequence {
    Sequence::new(vec![Mediator::Respond])
}

/// The HTTP response produced by a mediation flow.
#[derive(Debug)]
pub struct MediationResult {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    pub payload: Payload,
}

/// Execute a plan. The caller is responsible for filling `uri.var.*`,
/// `REST_URL_POSTFIX` and the initial payload.
pub async fn mediate<T: Transport + ?Sized>(
    plan: &MediationPlan,
    transport: &T,
    mut context: MessageContext,
) -> Result<MediationResult, SynapseError> {
    initialize_properties(&mut context);

    let mut executor = Executor {
        plan,
        transport,
        depth: 0,
        phase: Phase::In,
    };

    match executor.run_sequence(&plan.in_sequence, &mut context).await {
        Ok(Flow::Respond) => Ok(finish(context)),
        Ok(Flow::Drop) => Ok(drop_result()),
        Ok(Flow::Continue) => {
            executor.phase = Phase::Out;
            match executor
                .run_sequence(&plan.out_sequence, &mut context)
                .await
            {
                Ok(Flow::Respond) | Ok(Flow::Continue) => Ok(finish(context)),
                Ok(Flow::Drop) => Ok(drop_result()),
                Err(error) => fault(plan, transport, context, error).await,
            }
        }
        Err(error) => fault(plan, transport, context, error).await,
    }
}

fn initialize_properties(context: &mut MessageContext) {
    context.set_property(
        PropertyScope::Default,
        "HTTP_METHOD",
        context.method.clone(),
    );
    context.set_property(PropertyScope::Default, "REQUEST_URI", context.path.clone());
    if context
        .get_property(PropertyScope::Default, "REST_URL_POSTFIX")
        .is_none()
    {
        context.set_property(PropertyScope::Default, "REST_URL_POSTFIX", "/");
    }
    if context
        .get_property(PropertyScope::Axis2, "HTTP_SC")
        .is_none()
    {
        context.set_property(
            PropertyScope::Axis2,
            "HTTP_SC",
            context.http_status.to_string(),
        );
    }
}

async fn fault<T: Transport + ?Sized>(
    plan: &MediationPlan,
    transport: &T,
    mut context: MessageContext,
    error: SynapseError,
) -> Result<MediationResult, SynapseError> {
    let detail = error.to_string();
    tracing::warn!(
        plan = %plan.name,
        error = %detail,
        "synapse mediation failed; running fault sequence"
    );
    context.set_property(PropertyScope::Default, "ERROR_MESSAGE", detail.clone());
    context.set_property(PropertyScope::Default, "ERROR_CODE", "500");
    context.set_http_status(500);

    if plan.fault_sequence.is_empty() {
        return Ok(default_fault_response());
    }

    let mut executor = Executor {
        plan,
        transport,
        depth: 0,
        phase: Phase::Fault,
    };
    match executor
        .run_sequence(&plan.fault_sequence, &mut context)
        .await
    {
        Ok(Flow::Drop) => Ok(drop_result()),
        Ok(Flow::Respond) | Ok(Flow::Continue) => Ok(finish(context)),
        Err(fault_error) => Err(SynapseError::Mediation(format!(
            "fault sequence failed: {fault_error}; original error: {detail}"
        ))),
    }
}

fn finish(context: MessageContext) -> MediationResult {
    let mut headers = context.response_headers.clone();
    // The payload may have been re-serialized by payloadFactory/enrich.
    headers.remove(http::header::CONTENT_LENGTH);
    headers.remove(http::header::TRANSFER_ENCODING);
    headers.remove(http::header::CONTENT_ENCODING);
    if !context.payload.is_empty() && !headers.contains_key(CONTENT_TYPE) {
        if let Ok(value) = HeaderValue::from_str(context.payload.content_type()) {
            headers.insert(CONTENT_TYPE, value);
        }
    }
    let status = context.effective_status();
    let body = if matches!(status, 204 | 304) {
        Vec::new()
    } else {
        context.payload.to_bytes()
    };
    MediationResult {
        status,
        headers,
        body,
        payload: context.payload,
    }
}

fn drop_result() -> MediationResult {
    MediationResult {
        status: 200,
        headers: HeaderMap::new(),
        body: Vec::new(),
        payload: Payload::Empty,
    }
}

fn default_fault_response() -> MediationResult {
    let body = br#"{"fault":{"code":"500","message":"mediation failed"}}"#.to_vec();
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    MediationResult {
        status: 500,
        headers,
        body: body.clone(),
        payload: Payload::Bytes(body),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Respond,
    Drop,
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

struct Executor<'a, T: Transport + ?Sized> {
    plan: &'a MediationPlan,
    transport: &'a T,
    depth: usize,
    phase: Phase,
}

impl<'a, T: Transport + ?Sized> Executor<'a, T> {
    fn run_sequence<'b>(
        &'b mut self,
        sequence: &'b Sequence,
        context: &'b mut MessageContext,
    ) -> BoxFuture<'b, Result<Flow, SynapseError>> {
        Box::pin(async move {
            for mediator in &sequence.mediators {
                match self.run_mediator(mediator, context).await? {
                    Flow::Continue => {}
                    other => return Ok(other),
                }
            }
            Ok(Flow::Continue)
        })
    }

    async fn run_mediator(
        &mut self,
        mediator: &Mediator,
        context: &mut MessageContext,
    ) -> Result<Flow, SynapseError> {
        match mediator {
            Mediator::Log(log) => {
                log_mediator(log, context)?;
                Ok(Flow::Continue)
            }
            Mediator::Property(property) => {
                property_mediator(property, context)?;
                Ok(Flow::Continue)
            }
            Mediator::Filter(filter) => self.run_filter(filter, context).await,
            Mediator::Switch(switch) => self.run_switch(switch, context).await,
            Mediator::PayloadFactory(factory) => {
                payload_factory(factory, context)?;
                Ok(Flow::Continue)
            }
            Mediator::Enrich(enrich) => {
                enrich_mediator(enrich, context, self.phase)?;
                Ok(Flow::Continue)
            }
            Mediator::Call(call) => {
                self.call(call.endpoint.as_ref(), context).await?;
                Ok(Flow::Continue)
            }
            Mediator::Send(send) => {
                self.call(send.endpoint.as_ref(), context).await?;
                Ok(Flow::Continue)
            }
            Mediator::Respond => Ok(Flow::Respond),
            Mediator::Drop => Ok(Flow::Drop),
            Mediator::Header(header) => {
                header_mediator(header, context, self.phase)?;
                Ok(Flow::Continue)
            }
            Mediator::SequenceRef(name) => {
                if self.depth >= MAX_SEQUENCE_DEPTH {
                    return Err(SynapseError::Mediation(format!(
                        "sequence reference depth exceeded at {name}"
                    )));
                }
                let sequence = self
                    .plan
                    .named_sequences
                    .get(name)
                    .cloned()
                    .ok_or_else(|| SynapseError::UnknownSequence(name.clone()))?;
                self.depth += 1;
                let result = self.run_sequence(&sequence, context).await;
                self.depth -= 1;
                result
            }
        }
    }

    async fn run_filter(
        &mut self,
        filter: &FilterMediator,
        context: &mut MessageContext,
    ) -> Result<Flow, SynapseError> {
        if filter_matches(filter, context)? {
            self.run_sequence(&filter.then, context).await
        } else if let Some(otherwise) = &filter.otherwise {
            self.run_sequence(otherwise, context).await
        } else {
            Ok(Flow::Continue)
        }
    }

    async fn run_switch(
        &mut self,
        switch: &SwitchMediator,
        context: &mut MessageContext,
    ) -> Result<Flow, SynapseError> {
        let source = switch.source.evaluate(context)?;
        for case in &switch.cases {
            let matched = if let Some(regex) = &case.regex {
                Regex::new(regex)
                    .map_err(|error| SynapseError::Mediation(error.to_string()))?
                    .is_match(&source)
            } else if let Some(value) = &case.value {
                source == *value
            } else {
                false
            };
            if matched {
                return self.run_sequence(&case.sequence, context).await;
            }
        }
        if let Some(default) = &switch.default {
            return self.run_sequence(default, context).await;
        }
        Ok(Flow::Continue)
    }

    async fn call(
        &self,
        endpoint: Option<&EndpointRef>,
        context: &mut MessageContext,
    ) -> Result<(), SynapseError> {
        let base = self.plan.resolve_endpoint(endpoint)?;
        let url = build_request_url(&base, context)?;
        let method = Method::from_bytes(context.method.as_bytes()).map_err(|error| {
            SynapseError::Mediation(format!("invalid HTTP method {}: {error}", context.method))
        })?;

        let mut headers = context.request_headers.clone();
        strip_hop_by_hop(&mut headers);
        headers.remove(HOST);
        if !headers.contains_key(CONTENT_TYPE) && !context.payload.is_empty() {
            headers.insert(
                CONTENT_TYPE,
                HeaderValue::from_str(context.payload.content_type()).expect("static content type"),
            );
        }

        let response = self
            .transport
            .send(UpstreamRequest {
                method: method.as_str().to_owned(),
                url,
                headers,
                body: context.payload.to_bytes(),
            })
            .await?;

        context.http_status = response.status;
        context.set_property(PropertyScope::Axis2, "HTTP_SC", response.status.to_string());
        let content_type = response
            .headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        context.response_headers = response.headers;
        context.payload = Payload::from_bytes(content_type.as_deref(), response.body);
        Ok(())
    }
}

fn log_mediator(log: &LogMediator, context: &MessageContext) -> Result<(), SynapseError> {
    let mut properties = Vec::with_capacity(log.properties.len());
    for property in &log.properties {
        let value = if let Some(expression) = &property.expression {
            expression.evaluate(context)?
        } else {
            property.value.clone().unwrap_or_default()
        };
        properties.push((
            property
                .name
                .clone()
                .unwrap_or_else(|| "property".to_owned()),
            value,
        ));
    }

    let payload = if log.level == LogLevel::Full {
        truncate(&context.payload.as_text(), 4_096)
    } else {
        String::new()
    };
    let level = match log.level {
        LogLevel::Trace => Level::TRACE,
        LogLevel::Debug => Level::DEBUG,
        LogLevel::Warn => Level::WARN,
        LogLevel::Error => Level::ERROR,
        LogLevel::Simple | LogLevel::Custom | LogLevel::Full | LogLevel::Info => Level::INFO,
    };

    macro_rules! emit {
        ($level:expr) => {
            tracing::event!(
                target: "rusto2_synapse",
                $level,
                category = log.category.as_deref().unwrap_or(""),
                separator = log.separator.as_deref().unwrap_or(""),
                properties = ?properties,
                payload = %payload,
                "synapse log mediator"
            )
        };
    }
    match level {
        Level::TRACE => emit!(Level::TRACE),
        Level::DEBUG => emit!(Level::DEBUG),
        Level::INFO => emit!(Level::INFO),
        Level::WARN => emit!(Level::WARN),
        Level::ERROR => emit!(Level::ERROR),
    }
    Ok(())
}

fn property_mediator(
    property: &PropertyMediator,
    context: &mut MessageContext,
) -> Result<(), SynapseError> {
    if property.action == PropertyAction::Remove {
        context.remove_property(property.scope, &property.name);
        return Ok(());
    }

    let value = if let Some(expression) = &property.expression {
        expression.evaluate(context)?
    } else {
        property.value.clone().unwrap_or_default()
    };
    validate_property_value(property.data_type, &value, &property.name)?;
    context.set_property(property.scope, &property.name, &value);

    if property.scope == PropertyScope::Axis2 && property.name == "HTTP_SC" {
        context.http_status = value.parse().map_err(|_| {
            SynapseError::Mediation(format!("HTTP_SC is not a status code: {value}"))
        })?;
    }
    Ok(())
}

fn validate_property_value(
    data_type: PropertyType,
    value: &str,
    name: &str,
) -> Result<(), SynapseError> {
    let valid = match data_type {
        PropertyType::String | PropertyType::Om => true,
        PropertyType::Integer | PropertyType::Long | PropertyType::Short => {
            value.parse::<i64>().is_ok()
        }
        PropertyType::Boolean => {
            value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("false")
        }
        PropertyType::Double | PropertyType::Float => value.parse::<f64>().is_ok(),
        PropertyType::Json => serde_json::from_str::<serde_json::Value>(value).is_ok(),
    };
    if valid {
        Ok(())
    } else {
        Err(SynapseError::Mediation(format!(
            "property {name} is not a valid {:?}: {value}",
            data_type
        )))
    }
}

fn filter_matches(filter: &FilterMediator, context: &MessageContext) -> Result<bool, SynapseError> {
    if let Some(regex) = &filter.regex {
        let source = match &filter.source {
            Some(expression) => expression.evaluate(context)?,
            None => context.payload.as_text(),
        };
        Regex::new(regex)
            .map_err(|error| SynapseError::Mediation(format!("invalid filter regex: {error}")))
            .map(|regex| regex.is_match(&source))
    } else if let Some(xpath) = &filter.xpath {
        let value = Expression::XPath(xpath.clone()).evaluate(context)?;
        Ok(!value.is_empty())
    } else {
        Err(SynapseError::Mediation(
            "filter has neither regex nor xpath".to_owned(),
        ))
    }
}

fn payload_factory(
    factory: &PayloadFactoryMediator,
    context: &mut MessageContext,
) -> Result<(), SynapseError> {
    let mut values = Vec::with_capacity(factory.args.len());
    for argument in &factory.args {
        values.push(if let Some(expression) = &argument.expression {
            expression.evaluate(context)?
        } else {
            argument.value.clone().unwrap_or_default()
        });
    }

    let rendered = substitute_arguments(&factory.format, &values)?;
    let media_type = factory.media_type.to_ascii_lowercase();
    context.payload = if media_type.contains("json") {
        let value = serde_json::from_str(&rendered).map_err(|error| {
            SynapseError::Mediation(format!(
                "payloadFactory produced invalid JSON: {error}; payload: {}",
                truncate(&rendered, 256)
            ))
        })?;
        Payload::Json(value)
    } else if media_type.contains("xml") {
        roxmltree::Document::parse(&rendered).map_err(|error| {
            SynapseError::Mediation(format!(
                "payloadFactory produced invalid XML: {error}; payload: {}",
                truncate(&rendered, 256)
            ))
        })?;
        Payload::Xml(rendered)
    } else if media_type.contains("text") || media_type.contains("plain") {
        Payload::Text(rendered)
    } else {
        return Err(SynapseError::Mediation(format!(
            "payloadFactory media-type '{0}' is not supported",
            factory.media_type
        )));
    };
    Ok(())
}

fn substitute_arguments(format: &str, values: &[String]) -> Result<String, SynapseError> {
    let required = required_argument_count(format);
    if required > values.len() {
        return Err(SynapseError::Mediation(format!(
            "payloadFactory format needs ${required} but only {} arguments were provided",
            values.len()
        )));
    }

    let mut rendered = format.to_owned();
    for (index, value) in values.iter().enumerate().rev() {
        rendered = rendered.replace(&format!("${}", index + 1), value);
    }
    Ok(rendered)
}

fn required_argument_count(format: &str) -> usize {
    let bytes = format.as_bytes();
    let mut index = 0;
    let mut maximum = 0;
    while index < bytes.len() {
        if bytes[index] == b'$' && index + 1 < bytes.len() && bytes[index + 1].is_ascii_digit() {
            let start = index + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if let Ok(value) = format[start..end].parse::<usize>() {
                maximum = maximum.max(value);
            }
            index = end;
        } else {
            index += 1;
        }
    }
    maximum
}

fn enrich_mediator(
    enrich: &EnrichMediator,
    context: &mut MessageContext,
    phase: Phase,
) -> Result<(), SynapseError> {
    let source = match enrich.source_type {
        EnrichSourceType::Inline => enrich.source_inline.clone().unwrap_or_default(),
        EnrichSourceType::Body | EnrichSourceType::Envelope => context.payload.as_text(),
        EnrichSourceType::Property => {
            let name = enrich.source_name.as_deref().ok_or_else(|| {
                SynapseError::Mediation("enrich property source has no property name".to_owned())
            })?;
            context
                .get_property(PropertyScope::Default, name)
                .unwrap_or_default()
                .to_owned()
        }
        EnrichSourceType::Header => {
            let name = enrich.source_name.as_deref().ok_or_else(|| {
                SynapseError::Mediation("enrich header source has no header name".to_owned())
            })?;
            context
                .headers(phase)
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        }
    };

    match enrich.target_type {
        EnrichTargetType::Body | EnrichTargetType::Envelope => {
            apply_body_enrichment(enrich, context, source)
        }
        EnrichTargetType::Property => {
            let name = enrich.target_name.as_deref().ok_or_else(|| {
                SynapseError::Mediation("enrich property target has no property name".to_owned())
            })?;
            if enrich.target_action == EnrichAction::Remove {
                context.remove_property(PropertyScope::Default, name);
            } else {
                context.set_property(PropertyScope::Default, name, source);
            }
            Ok(())
        }
        EnrichTargetType::Header => {
            let name = enrich.target_name.as_deref().ok_or_else(|| {
                SynapseError::Mediation("enrich header target has no header name".to_owned())
            })?;
            if enrich.target_action == EnrichAction::Remove {
                context.remove_header(phase, name);
                Ok(())
            } else {
                context.set_header(phase, name, &source)
            }
        }
    }
}

fn apply_body_enrichment(
    enrich: &EnrichMediator,
    context: &mut MessageContext,
    source: String,
) -> Result<(), SynapseError> {
    match enrich.target_action {
        EnrichAction::Replace => {
            context.payload =
                Payload::from_bytes(enrich.source_media_type.as_deref(), source.into_bytes());
            Ok(())
        }
        EnrichAction::Remove => {
            context.payload = Payload::Empty;
            Ok(())
        }
        EnrichAction::Append | EnrichAction::Prepend => {
            let append = enrich.target_action == EnrichAction::Append;
            match (
                &context.payload,
                serde_json::from_str::<serde_json::Value>(&source),
            ) {
                (Payload::Json(current), Ok(incoming)) => {
                    let combined = combine_json(current, incoming, append)?;
                    context.payload = Payload::Json(combined);
                    Ok(())
                }
                (Payload::Json(_), Err(_)) => Err(SynapseError::Mediation(
                    "enrich append/prepend of non-JSON into a JSON body is not supported"
                        .to_owned(),
                )),
                (_, _) => {
                    let current = context.payload.as_text();
                    let combined = if append {
                        format!("{current}{source}")
                    } else {
                        format!("{source}{current}")
                    };
                    context.payload = Payload::Text(combined);
                    Ok(())
                }
            }
        }
        EnrichAction::Child => Err(SynapseError::Mediation(
            "enrich action 'child' is not supported for JSON/XML bodies yet".to_owned(),
        )),
    }
}

fn combine_json(
    current: &serde_json::Value,
    incoming: serde_json::Value,
    append: bool,
) -> Result<serde_json::Value, SynapseError> {
    match (current, incoming) {
        (serde_json::Value::Array(current), serde_json::Value::Array(mut incoming)) => {
            if append {
                let mut result = current.clone();
                result.append(&mut incoming);
                Ok(serde_json::Value::Array(result))
            } else {
                incoming.extend(current.iter().cloned());
                Ok(serde_json::Value::Array(incoming))
            }
        }
        (serde_json::Value::Array(current), incoming) => {
            let mut result = current.clone();
            if append {
                result.push(incoming);
            } else {
                result.insert(0, incoming);
            }
            Ok(serde_json::Value::Array(result))
        }
        _ => Err(SynapseError::Mediation(
            "enrich append/prepend requires an array JSON body".to_owned(),
        )),
    }
}

fn header_mediator(
    header: &HeaderMediator,
    context: &mut MessageContext,
    phase: Phase,
) -> Result<(), SynapseError> {
    if header.action == HeaderAction::Remove {
        context.remove_header(phase, &header.name);
        return Ok(());
    }
    let value = if let Some(expression) = &header.expression {
        expression.evaluate(context)?
    } else {
        header.value.clone().unwrap_or_default()
    };
    context.set_header(phase, &header.name, &value)
}

fn build_request_url(base: &str, context: &MessageContext) -> Result<String, SynapseError> {
    let mut url = url::Url::parse(base).map_err(|error| {
        SynapseError::Mediation(format!("invalid endpoint URL {base}: {error}"))
    })?;
    let postfix = context.rest_postfix();
    if !postfix.is_empty() {
        let base_path = url.path().trim_end_matches('/');
        let path = if postfix == "/" {
            format!("{base_path}/")
        } else if base_path.is_empty() {
            postfix.to_owned()
        } else {
            format!("{base_path}{postfix}")
        };
        url.set_path(&path);
    }
    if url.query().is_none() {
        if let Some(query) = context.query.as_deref().filter(|query| !query.is_empty()) {
            url.set_query(Some(query));
        }
    }
    Ok(url.to_string())
}

fn strip_hop_by_hop(headers: &mut HeaderMap) {
    const HOP_BY_HOP: &[&str] = &[
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "content-length",
    ];
    for name in HOP_BY_HOP {
        headers.remove(*name);
    }
}

fn validate_http_url(url: &str) -> Result<(), SynapseError> {
    let parsed = url::Url::parse(url)
        .map_err(|error| SynapseError::Mediation(format!("invalid endpoint URL {url}: {error}")))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host().is_none() {
        return Err(SynapseError::Mediation(format!(
            "endpoint URL must be absolute http(s): {url}"
        )));
    }
    Ok(())
}

fn validate_sequence(
    plan: &MediationPlan,
    sequence: &Sequence,
    depth: usize,
) -> Result<(), SynapseError> {
    if depth > MAX_SEQUENCE_DEPTH {
        return Err(SynapseError::Mediation(format!(
            "sequence nesting is deeper than {MAX_SEQUENCE_DEPTH}"
        )));
    }
    for mediator in &sequence.mediators {
        match mediator {
            Mediator::SequenceRef(name) => {
                let referenced = plan
                    .named_sequences
                    .get(name)
                    .ok_or_else(|| SynapseError::UnknownSequence(name.clone()))?;
                validate_sequence(plan, referenced, depth + 1)?;
            }
            Mediator::Call(call) => validate_call(plan, call)?,
            Mediator::Send(send) => validate_send(plan, send)?,
            Mediator::Filter(filter) => {
                if let Some(regex) = &filter.regex {
                    Regex::new(regex).map_err(|error| {
                        SynapseError::Mediation(format!("invalid filter regex '{regex}': {error}"))
                    })?;
                }
                validate_sequence(plan, &filter.then, depth + 1)?;
                if let Some(otherwise) = &filter.otherwise {
                    validate_sequence(plan, otherwise, depth + 1)?;
                }
            }
            Mediator::Switch(switch) => {
                for case in &switch.cases {
                    if let Some(regex) = &case.regex {
                        Regex::new(regex).map_err(|error| {
                            SynapseError::Mediation(format!(
                                "invalid switch regex '{regex}': {error}"
                            ))
                        })?;
                    }
                    validate_sequence(plan, &case.sequence, depth + 1)?;
                }
                if let Some(default) = &switch.default {
                    validate_sequence(plan, default, depth + 1)?;
                }
            }
            Mediator::PayloadFactory(factory) => {
                if required_argument_count(&factory.format) > factory.args.len() {
                    return Err(SynapseError::Mediation(format!(
                        "payloadFactory format needs {} arguments but only {} were provided",
                        required_argument_count(&factory.format),
                        factory.args.len()
                    )));
                }
            }
            Mediator::Enrich(enrich) => {
                if enrich.target_action == EnrichAction::Child {
                    return Err(SynapseError::Mediation(
                        "enrich action 'child' is not supported".to_owned(),
                    ));
                }
                if matches!(
                    enrich.target_type,
                    EnrichTargetType::Property | EnrichTargetType::Header
                ) && enrich.target_name.is_none()
                {
                    return Err(SynapseError::Mediation(
                        "enrich property/header target needs a name".to_owned(),
                    ));
                }
                if enrich.source_type == EnrichSourceType::Property && enrich.source_name.is_none()
                {
                    return Err(SynapseError::Mediation(
                        "enrich property source needs a name".to_owned(),
                    ));
                }
            }
            Mediator::Header(header) => {
                HeaderName::from_bytes(header.name.as_bytes()).map_err(|error| {
                    SynapseError::Mediation(format!("invalid header name {}: {error}", header.name))
                })?;
            }
            Mediator::Log(_) | Mediator::Property(_) | Mediator::Respond | Mediator::Drop => {}
        }
    }
    Ok(())
}

fn validate_call(plan: &MediationPlan, call: &CallMediator) -> Result<(), SynapseError> {
    if !call.blocking {
        return Err(SynapseError::Mediation(
            "non-blocking <call> is not supported; use blocking=\"true\"".to_owned(),
        ));
    }
    validate_endpoint_ref(plan, call.endpoint.as_ref())
}

fn validate_send(plan: &MediationPlan, send: &SendMediator) -> Result<(), SynapseError> {
    validate_endpoint_ref(plan, send.endpoint.as_ref())
}

fn validate_endpoint_ref(
    plan: &MediationPlan,
    endpoint: Option<&EndpointRef>,
) -> Result<(), SynapseError> {
    match endpoint {
        Some(EndpointRef::Inline(url)) => validate_http_url(url),
        Some(EndpointRef::Named(name)) => {
            if plan.endpoints.contains_key(name) {
                Ok(())
            } else {
                Err(SynapseError::UnknownEndpoint(name.clone()))
            }
        }
        None => {
            if plan.default_upstream.is_some() {
                Ok(())
            } else {
                Err(SynapseError::Mediation(format!(
                    "{} uses the default endpoint but no endpoint is configured",
                    plan.name
                )))
            }
        }
    }
}

fn truncate(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_owned();
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &value[..end])
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use http::{HeaderMap, HeaderValue};

    use super::*;

    #[derive(Default)]
    struct FakeTransport {
        requests: Mutex<Vec<UpstreamRequest>>,
        response: Mutex<Option<(u16, HeaderMap, Vec<u8>)>>,
    }

    #[async_trait]
    impl Transport for FakeTransport {
        async fn send(
            &self,
            request: UpstreamRequest,
        ) -> Result<crate::transport::UpstreamResponse, SynapseError> {
            self.requests.lock().unwrap().push(request);
            let (status, headers, body) = self.response.lock().unwrap().clone().unwrap_or((
                200,
                HeaderMap::new(),
                br#"{"ok":true}"#.to_vec(),
            ));
            Ok(crate::transport::UpstreamResponse {
                status,
                headers,
                body,
            })
        }
    }

    fn plan(
        in_sequence: Sequence,
        out_sequence: Sequence,
        fault_sequence: Sequence,
    ) -> MediationPlan {
        MediationPlan {
            name: "test".to_owned(),
            api_path: "/test/1.0.0".to_owned(),
            default_upstream: Some("http://backend:8080".to_owned()),
            in_sequence,
            out_sequence,
            fault_sequence,
            named_sequences: HashMap::new(),
            endpoints: HashMap::new(),
        }
    }

    fn context() -> MessageContext {
        let mut context = MessageContext::new(
            "GET",
            "/test/1.0.0/items/42",
            Some("q=1".to_owned()),
            HeaderMap::new(),
            Payload::Empty,
        );
        context.set_property(PropertyScope::Default, "REST_URL_POSTFIX", "/items/42");
        context.set_property(PropertyScope::Default, "uri.var.id", "42");
        context
    }

    #[tokio::test]
    async fn runs_property_filter_switch_and_payload_factory() {
        let plan = plan(
            Sequence::new(vec![
                Mediator::Property(PropertyMediator {
                    name: "kind".to_owned(),
                    value: Some("special".to_owned()),
                    expression: None,
                    scope: PropertyScope::Default,
                    action: PropertyAction::Set,
                    data_type: PropertyType::String,
                }),
                Mediator::Filter(FilterMediator {
                    source: Some(Expression::parse("get-property('kind')").unwrap()),
                    regex: Some("^special$".to_owned()),
                    xpath: None,
                    then: Sequence::new(vec![Mediator::Property(PropertyMediator {
                        name: "branch".to_owned(),
                        value: Some("then".to_owned()),
                        expression: None,
                        scope: PropertyScope::Default,
                        action: PropertyAction::Set,
                        data_type: PropertyType::String,
                    })]),
                    otherwise: None,
                }),
                Mediator::PayloadFactory(PayloadFactoryMediator {
                    media_type: "application/json".to_owned(),
                    format: r#"{"id":"$1","branch":"$2"}"#.to_owned(),
                    args: vec![
                        crate::sequence::PayloadArg {
                            expression: Some(
                                Expression::parse("get-property('uri.var.id')").unwrap(),
                            ),
                            value: None,
                        },
                        crate::sequence::PayloadArg {
                            expression: Some(Expression::parse("get-property('branch')").unwrap()),
                            value: None,
                        },
                    ],
                }),
            ]),
            Sequence::new(vec![
                Mediator::Header(HeaderMediator {
                    name: "X-Test".to_owned(),
                    value: Some("yes".to_owned()),
                    expression: None,
                    action: HeaderAction::Set,
                    scope: crate::sequence::HeaderScope::Transport,
                }),
                Mediator::Respond,
            ]),
            Sequence::default(),
        );
        plan.validate().unwrap();

        let transport = FakeTransport::default();
        let result = mediate(&plan, &transport, context())
            .await
            .expect("mediation");
        assert_eq!(result.status, 200);
        assert_eq!(
            result.payload,
            Payload::Json(serde_json::json!({"id": "42", "branch": "then"}))
        );
        assert_eq!(result.headers.get("x-test").unwrap(), "yes");
    }

    #[tokio::test]
    async fn call_then_out_sequence_uses_backend_response() {
        let transport = FakeTransport::default();
        *transport.response.lock().unwrap() = Some((
            201,
            HeaderMap::from_iter([(CONTENT_TYPE, HeaderValue::from_static("application/json"))]),
            br#"{"created":true}"#.to_vec(),
        ));

        let plan = plan(
            Sequence::new(vec![Mediator::Call(CallMediator {
                endpoint: None,
                blocking: true,
            })]),
            Sequence::new(vec![
                Mediator::Header(HeaderMediator {
                    name: "X-Out".to_owned(),
                    value: Some("yes".to_owned()),
                    expression: None,
                    action: HeaderAction::Set,
                    scope: crate::sequence::HeaderScope::Transport,
                }),
                Mediator::Respond,
            ]),
            Sequence::default(),
        );
        plan.validate().unwrap();

        let result = mediate(&plan, &transport, context())
            .await
            .expect("mediation");
        assert_eq!(result.status, 201);
        assert_eq!(result.headers.get("x-out").unwrap(), "yes");
        let request = &transport.requests.lock().unwrap()[0];
        assert_eq!(request.url, "http://backend:8080/items/42?q=1");
        assert_eq!(request.method, "GET");
    }

    #[tokio::test]
    async fn fault_sequence_runs_on_transport_error() {
        struct Failing;
        #[async_trait]
        impl Transport for Failing {
            async fn send(
                &self,
                _: UpstreamRequest,
            ) -> Result<crate::transport::UpstreamResponse, SynapseError> {
                Err(SynapseError::Transport("connection refused".to_owned()))
            }
        }

        let plan = plan(
            Sequence::new(vec![Mediator::Call(CallMediator {
                endpoint: None,
                blocking: true,
            })]),
            Sequence::default(),
            Sequence::new(vec![
                Mediator::Property(PropertyMediator {
                    name: "HTTP_SC".to_owned(),
                    value: Some("502".to_owned()),
                    expression: None,
                    scope: PropertyScope::Axis2,
                    action: PropertyAction::Set,
                    data_type: PropertyType::Integer,
                }),
                Mediator::PayloadFactory(PayloadFactoryMediator {
                    media_type: "application/json".to_owned(),
                    format: r#"{"error":"upstream"}"#.to_owned(),
                    args: Vec::new(),
                }),
                Mediator::Respond,
            ]),
        );
        plan.validate().unwrap();

        let result = mediate(&plan, &Failing, context()).await.expect("fault");
        assert_eq!(result.status, 502);
        assert_eq!(
            result.payload,
            Payload::Json(serde_json::json!({"error": "upstream"}))
        );
    }

    #[test]
    fn validates_payload_factory_argument_count() {
        let plan = plan(
            Sequence::new(vec![Mediator::PayloadFactory(PayloadFactoryMediator {
                media_type: "application/json".to_owned(),
                format: r#"{"id":"$1","name":"$2"}"#.to_owned(),
                args: vec![crate::sequence::PayloadArg {
                    expression: None,
                    value: Some("one".to_owned()),
                }],
            })]),
            Sequence::default(),
            Sequence::default(),
        );
        assert!(plan.validate().is_err());
    }
}
