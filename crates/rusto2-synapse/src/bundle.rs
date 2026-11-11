use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use http::Method;

use crate::{
    apim::ApiDefinition,
    error::SynapseError,
    sequence::{
        CallMediator, EndpointRef, FilterMediator, HeaderMediator, LogMediator, Mediator,
        PayloadFactoryMediator, SendMediator, Sequence, SwitchMediator,
    },
    synapse::{ApiArtifact, EndpointDefinition, SynapseDocument},
};

/// Everything loaded from WSO2 artifacts, before it is converted into gateway
/// routes.
#[derive(Debug, Default)]
pub struct ArtifactBundle {
    pub apis: Vec<ApiArtifact>,
    pub definitions: Vec<ApiDefinition>,
    pub sequences: HashMap<String, Sequence>,
    pub endpoints: HashMap<String, String>,
    pub ignored: Vec<String>,
    pub sources: Vec<String>,
}

impl ArtifactBundle {
    pub fn load_paths(paths: &[String]) -> Result<Self, SynapseError> {
        let mut bundle = Self::default();
        for path in paths {
            bundle.merge(Self::load_path(Path::new(path))?);
        }
        bundle.validate()?;
        Ok(bundle)
    }

    pub fn load_path(path: &Path) -> Result<Self, SynapseError> {
        if !path.exists() {
            return Err(SynapseError::NotFound(path.to_path_buf()));
        }
        let mut bundle = Self::default();
        if path.is_dir() {
            let mut entries = std::fs::read_dir(path)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<Vec<_>, _>>()?;
            entries.sort();
            for entry in entries {
                if entry.is_dir() {
                    bundle.merge(Self::load_path(&entry)?);
                } else {
                    bundle.load_file(&entry, false)?;
                }
            }
        } else {
            bundle.load_file(path, true)?;
        }
        Ok(bundle)
    }

    fn load_file(&mut self, path: &Path, strict: bool) -> Result<(), SynapseError> {
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();

        match extension.as_str() {
            "car" | "zip" => {
                let package = crate::car::read(path)?;
                for (name, document) in package.documents {
                    self.merge_document(format!("{}!{name}", path.display()), document)?;
                }
                for (name, definition) in package.definitions {
                    self.merge_definition(format!("{}!{name}", path.display()), definition)?;
                }
                self.ignored.extend(
                    package
                        .ignored
                        .into_iter()
                        .map(|artifact| format!("{}!{artifact}", path.display())),
                );
                self.sources.push(path.display().to_string());
            }
            "xml" => {
                let text = std::fs::read_to_string(path)?;
                let document = crate::synapse::parse_document(&text)
                    .map_err(|error| SynapseError::Xml(format!("{}: {error}", path.display())))?;
                if document.apis.is_empty()
                    && document.sequences.is_empty()
                    && document.endpoints.is_empty()
                {
                    if strict {
                        return Err(SynapseError::UnsupportedArtifact(format!(
                            "{}: {}",
                            path.display(),
                            document
                                .ignored
                                .first()
                                .cloned()
                                .unwrap_or_else(|| "no supported artifact".to_owned())
                        )));
                    }
                    self.ignored.extend(
                        document
                            .ignored
                            .into_iter()
                            .map(|artifact| format!("{}: {artifact}", path.display())),
                    );
                    return Ok(());
                }
                self.merge_document(path.display().to_string(), document)?;
                self.sources.push(path.display().to_string());
            }
            "yaml" | "yml" | "json" => {
                let text = std::fs::read_to_string(path)?;
                match crate::apim::parse_yaml(&text) {
                    Ok(definition) => {
                        self.merge_definition(path.display().to_string(), definition)?;
                        self.sources.push(path.display().to_string());
                    }
                    Err(error) if strict => {
                        return Err(SynapseError::ApiDefinition(format!(
                            "{}: {error}",
                            path.display()
                        )));
                    }
                    Err(_) => {}
                }
            }
            other if strict => {
                return Err(SynapseError::UnsupportedArtifact(format!(
                    "{}: unsupported file extension '{other}'",
                    path.display()
                )));
            }
            _ => {}
        }
        Ok(())
    }

    pub fn merge(&mut self, other: Self) {
        self.apis.extend(other.apis);
        self.definitions.extend(other.definitions);
        for (name, sequence) in other.sequences {
            // Later artifacts win only when the name is new; duplicates are
            // caught by validate().
            self.sequences.entry(name).or_insert(sequence);
        }
        for (name, url) in other.endpoints {
            self.endpoints.entry(name).or_insert(url);
        }
        self.ignored.extend(other.ignored);
        self.sources.extend(other.sources);
    }

    fn merge_document(
        &mut self,
        source: String,
        document: SynapseDocument,
    ) -> Result<(), SynapseError> {
        let SynapseDocument {
            apis,
            sequences,
            endpoints,
            ignored,
        } = document;
        self.apis.extend(apis);
        for sequence in sequences {
            if let Some(name) = sequence.name.clone() {
                if self.sequences.insert(name.clone(), sequence).is_some() {
                    return Err(SynapseError::Duplicate {
                        kind: "sequence",
                        name: format!("{name} ({source})"),
                    });
                }
            }
        }
        for EndpointDefinition { name, url } in endpoints {
            if self.endpoints.insert(name.clone(), url).is_some() {
                return Err(SynapseError::Duplicate {
                    kind: "endpoint",
                    name: format!("{name} ({source})"),
                });
            }
        }
        self.ignored.extend(
            ignored
                .into_iter()
                .map(|artifact| format!("{source}: {artifact}")),
        );
        Ok(())
    }

    fn merge_definition(
        &mut self,
        source: String,
        definition: ApiDefinition,
    ) -> Result<(), SynapseError> {
        if self
            .definitions
            .iter()
            .any(|existing| existing.name == definition.name)
        {
            return Err(SynapseError::Duplicate {
                kind: "APIM API",
                name: format!("{} ({source})", definition.name),
            });
        }
        self.definitions.push(definition);
        Ok(())
    }

    pub fn validate(&self) -> Result<(), SynapseError> {
        let mut api_names = HashMap::new();
        for api in &self.apis {
            if api_names.insert(api.name.clone(), ()).is_some() {
                return Err(SynapseError::Duplicate {
                    kind: "Synapse API",
                    name: api.name.clone(),
                });
            }
            if api.resources.is_empty() {
                return Err(SynapseError::Xml(format!(
                    "API {} has no resources",
                    api.name
                )));
            }
            validate_sequence_option(self, api.in_sequence.as_ref(), &api.name)?;
            validate_sequence_option(self, api.out_sequence.as_ref(), &api.name)?;
            validate_sequence_option(self, api.fault_sequence.as_ref(), &api.name)?;
            for resource in &api.resources {
                for method in &resource.methods {
                    Method::from_bytes(method.as_bytes()).map_err(|error| {
                        SynapseError::Xml(format!(
                            "API {} resource {} has invalid method {method}: {error}",
                            api.name, resource.uri_template
                        ))
                    })?;
                }
                validate_sequence_option(self, resource.in_sequence.as_ref(), &api.name)?;
                validate_sequence_option(self, resource.out_sequence.as_ref(), &api.name)?;
                validate_sequence_option(self, resource.fault_sequence.as_ref(), &api.name)?;
            }
        }

        let mut definition_names = HashMap::new();
        for definition in &self.definitions {
            if definition_names
                .insert(definition.name.clone(), ())
                .is_some()
            {
                return Err(SynapseError::Duplicate {
                    kind: "APIM API",
                    name: definition.name.clone(),
                });
            }
            if definition.operations.is_empty() {
                return Err(SynapseError::ApiDefinition(format!(
                    "API {} has no operations",
                    definition.name
                )));
            }
            let mut seen = HashMap::new();
            for operation in &definition.operations {
                let key = (operation.target.clone(), operation.verb.clone());
                if seen.insert(key.clone(), ()).is_some() {
                    return Err(SynapseError::ApiDefinition(format!(
                        "API {} declares {} {} twice",
                        definition.name, operation.verb, operation.target
                    )));
                }
                if !operation.target.starts_with('/') {
                    return Err(SynapseError::ApiDefinition(format!(
                        "API {} operation target must start with '/': {}",
                        definition.name, operation.target
                    )));
                }
                Method::from_bytes(operation.verb.as_bytes()).map_err(|error| {
                    SynapseError::ApiDefinition(format!(
                        "API {} has invalid method {}: {error}",
                        definition.name, operation.verb
                    ))
                })?;
            }
        }

        Ok(())
    }
}

fn validate_sequence_option(
    bundle: &ArtifactBundle,
    sequence: Option<&Sequence>,
    api_name: &str,
) -> Result<(), SynapseError> {
    let Some(sequence) = sequence else {
        return Ok(());
    };
    validate_sequence(bundle, sequence, api_name, 0)
}

fn validate_sequence(
    bundle: &ArtifactBundle,
    sequence: &Sequence,
    api_name: &str,
    depth: usize,
) -> Result<(), SynapseError> {
    if depth > 32 {
        return Err(SynapseError::Mediation(format!(
            "sequence nesting is deeper than 32 in API {api_name}"
        )));
    }
    for mediator in &sequence.mediators {
        match mediator {
            Mediator::SequenceRef(name) => {
                let referenced = bundle.sequences.get(name).ok_or_else(|| {
                    SynapseError::UnknownSequence(format!("{name} (API {api_name})"))
                })?;
                validate_sequence(bundle, referenced, api_name, depth + 1)?;
            }
            Mediator::Call(CallMediator { endpoint, .. })
            | Mediator::Send(SendMediator { endpoint }) => {
                validate_endpoint(bundle, endpoint.as_ref(), api_name)?;
            }
            Mediator::Filter(FilterMediator {
                then, otherwise, ..
            }) => {
                validate_sequence(bundle, then, api_name, depth + 1)?;
                if let Some(otherwise) = otherwise {
                    validate_sequence(bundle, otherwise, api_name, depth + 1)?;
                }
            }
            Mediator::Switch(SwitchMediator { cases, default, .. }) => {
                for case in cases {
                    validate_sequence(bundle, &case.sequence, api_name, depth + 1)?;
                }
                if let Some(default) = default {
                    validate_sequence(bundle, default, api_name, depth + 1)?;
                }
            }
            Mediator::Log(LogMediator { .. })
            | Mediator::Property(_)
            | Mediator::PayloadFactory(PayloadFactoryMediator { .. })
            | Mediator::Enrich(_)
            | Mediator::Respond
            | Mediator::Drop
            | Mediator::Header(HeaderMediator { .. }) => {}
        }
    }
    Ok(())
}

fn validate_endpoint(
    bundle: &ArtifactBundle,
    endpoint: Option<&EndpointRef>,
    api_name: &str,
) -> Result<(), SynapseError> {
    match endpoint {
        Some(EndpointRef::Named(name)) => {
            if !bundle.endpoints.contains_key(name) {
                return Err(SynapseError::UnknownEndpoint(format!(
                    "{name} (API {api_name})"
                )));
            }
        }
        Some(EndpointRef::Inline(url)) => {
            let parsed = url::Url::parse(url).map_err(|error| {
                SynapseError::UnsupportedEndpoint(format!("{url} in API {api_name}: {error}"))
            })?;
            if !matches!(parsed.scheme(), "http" | "https") || parsed.host().is_none() {
                return Err(SynapseError::UnsupportedEndpoint(format!(
                    "{url} in API {api_name} is not an absolute http(s) URL"
                )));
            }
        }
        None => {}
    }
    Ok(())
}

/// Resolve a path relative to the current directory, keeping the original error
/// informative when the file does not exist.
pub fn resolve(path: impl AsRef<Path>) -> Result<PathBuf, SynapseError> {
    let path = path.as_ref();
    if path.exists() {
        Ok(path.to_path_buf())
    } else {
        Err(SynapseError::NotFound(path.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn loads_apim_and_synapse_from_a_car() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/wso2");
        let directory = tempfile::tempdir().expect("tempdir");
        let car = directory.path().join("demo.car");
        let file = std::fs::File::create(&car).expect("car");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();

        for (name, source) in [
            (
                "artifacts/PizzaShackAPI/api.yaml",
                root.join("apim/pizzashack-api.yaml"),
            ),
            (
                "synapse-configs/default/api/MediationDemo.xml",
                root.join("synapse/MediationDemo.xml"),
            ),
            (
                "synapse-configs/default/sequences/CommonSequences.xml",
                root.join("synapse/CommonSequences.xml"),
            ),
        ] {
            zip.start_file(name, options).expect("zip entry");
            zip.write_all(&std::fs::read(source).expect("sample"))
                .expect("write");
        }
        zip.finish().expect("finish car");

        let bundle = ArtifactBundle::load_path(&car).expect("bundle");
        assert_eq!(bundle.definitions.len(), 1);
        assert_eq!(bundle.apis.len(), 1);
        assert!(bundle.sequences.contains_key("CommonFault"));
        assert_eq!(
            bundle.endpoints.get("HttpbinEndpoint").map(String::as_str),
            Some("http://httpbin:8080")
        );

        let api = &bundle.apis[0];
        let resource = api
            .resources
            .iter()
            .find(|resource| resource.uri_template == "/anything/*")
            .expect("resource");
        let plan = crate::MediationPlan::from_api(api, resource, &bundle, None).expect("plan");
        assert!(matches!(
            plan.fault_sequence.mediators.first(),
            Some(crate::sequence::Mediator::SequenceRef(name)) if name == "CommonFault"
        ));
        assert!(plan.named_sequences.contains_key("CommonFault"));
    }
}
