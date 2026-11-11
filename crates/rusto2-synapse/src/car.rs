use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use crate::{apim::ApiDefinition, error::SynapseError, synapse::SynapseDocument};

/// A Carbon Application archive unpacked into the artifacts RustO2 understands.
#[derive(Debug, Default)]
pub struct CarPackage {
    pub documents: Vec<(String, SynapseDocument)>,
    pub definitions: Vec<(String, ApiDefinition)>,
    pub ignored: Vec<String>,
}

const MAX_ENTRIES: usize = 4_096;
const MAX_ENTRY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Read a `.car` (a plain ZIP) without extracting it to disk.
///
/// Real CAR files include artifact types RustO2 does not execute yet (proxy
/// services, tasks, message stores). Those are recorded in `ignored`; API,
/// sequence and endpoint artifacts are parsed. Files that live under
/// `synapse-configs/default/api` or `.../sequences` must parse as their
/// expected type, otherwise loading fails.
pub fn read(path: &Path) -> Result<CarPackage, SynapseError> {
    let file = File::open(path).map_err(|error| SynapseError::Car {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| SynapseError::Car {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;

    if archive.len() > MAX_ENTRIES {
        return Err(SynapseError::Car {
            path: path.to_path_buf(),
            reason: format!("archive has more than {MAX_ENTRIES} entries"),
        });
    }

    let mut package = CarPackage::default();
    let mut total = 0u64;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| SynapseError::Car {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
        if entry.is_dir() {
            continue;
        }
        let name = entry
            .name()
            .map_err(|error| SynapseError::Car {
                path: path.to_path_buf(),
                reason: error.to_string(),
            })?
            .into_owned();
        if entry.size() > MAX_ENTRY_BYTES {
            return Err(SynapseError::Car {
                path: path.to_path_buf(),
                reason: format!("{name} exceeds {MAX_ENTRY_BYTES} bytes"),
            });
        }
        total = total.saturating_add(entry.size());
        if total > MAX_TOTAL_BYTES {
            return Err(SynapseError::Car {
                path: path.to_path_buf(),
                reason: format!("uncompressed contents exceed {MAX_TOTAL_BYTES} bytes"),
            });
        }

        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".xml") {
            let mut text = String::new();
            entry
                .read_to_string(&mut text)
                .map_err(|error| SynapseError::Car {
                    path: path.to_path_buf(),
                    reason: format!("{name}: {error}"),
                })?;
            let document =
                crate::synapse::parse_document(&text).map_err(|error| SynapseError::Car {
                    path: path.to_path_buf(),
                    reason: format!("{name}: {error}"),
                })?;

            if document.apis.is_empty()
                && document.sequences.is_empty()
                && document.endpoints.is_empty()
            {
                if is_api_path(&name) || is_sequence_path(&name) || is_endpoint_path(&name) {
                    return Err(SynapseError::Car {
                        path: path.to_path_buf(),
                        reason: format!("{name} is not a supported Synapse artifact"),
                    });
                }
                package.ignored.extend(
                    document
                        .ignored
                        .into_iter()
                        .map(|artifact| format!("{name}: {artifact}")),
                );
                continue;
            }

            package.ignored.extend(
                document
                    .ignored
                    .iter()
                    .map(|artifact| format!("{name}: {artifact}")),
            );
            package.documents.push((name, document));
            continue;
        }

        if lower.ends_with(".yaml") || lower.ends_with(".yml") {
            let mut text = String::new();
            entry
                .read_to_string(&mut text)
                .map_err(|error| SynapseError::Car {
                    path: path.to_path_buf(),
                    reason: format!("{name}: {error}"),
                })?;
            match crate::apim::parse_yaml(&text) {
                Ok(definition) => package.definitions.push((name, definition)),
                Err(error) if is_api_path(&name) => {
                    return Err(SynapseError::Car {
                        path: path.to_path_buf(),
                        reason: format!("{name}: {error}"),
                    });
                }
                Err(_) => {}
            }
        }
    }

    if package.documents.is_empty() && package.definitions.is_empty() {
        return Err(SynapseError::Car {
            path: PathBuf::from(path),
            reason: "archive contains no API, sequence or endpoint artifacts".to_owned(),
        });
    }

    Ok(package)
}

fn is_api_path(name: &str) -> bool {
    name.replace('\\', "/").contains("/api/")
}

fn is_sequence_path(name: &str) -> bool {
    name.replace('\\', "/").contains("/sequences/")
}

fn is_endpoint_path(name: &str) -> bool {
    name.replace('\\', "/").contains("/endpoints/")
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn reads_a_generated_car() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("test.car");
        let file = File::create(&path).expect("file");
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "synapse-configs/default/api/TestAPI.xml",
            zip::write::SimpleFileOptions::default(),
        )
        .expect("start api");
        zip.write_all(
            br#"<api xmlns="http://ws.apache.org/ns/synapse" name="TestAPI" context="/test" version="1.0.0">
                  <resource methods="GET" uri-template="/x"><inSequence><respond/></inSequence></resource>
                </api>"#,
        )
        .expect("write api");
        zip.start_file(
            "synapse-configs/default/sequences/Common.xml",
            zip::write::SimpleFileOptions::default(),
        )
        .expect("start sequence");
        zip.write_all(
            br#"<sequence xmlns="http://ws.apache.org/ns/synapse" name="Common"><log/><respond/></sequence>"#,
        )
        .expect("write sequence");
        zip.finish().expect("finish");

        let package = read(&path).expect("package");
        assert_eq!(package.documents.len(), 2);
        assert_eq!(package.documents[0].1.apis.len(), 1);
        assert_eq!(package.documents[1].1.sequences.len(), 1);
    }
}
