use crate::error::SynapseError;

/// One segment of a Synapse URI template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Literal(String),
    Param(String),
    /// Synapse `*` suffix: matches the rest of the path.
    Wildcard,
}

/// A compiled Synapse `uri-template` such as `/menu/{id}` or `/files/*`.
///
/// Matching is segment based, so `/menu` never matches `/menu/../admin`. Path
/// parameters are returned as `(name, value)` pairs and become `uri.var.*`
/// message properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathPattern {
    source: String,
    segments: Vec<Segment>,
}

impl PathPattern {
    pub fn parse(template: &str) -> Result<Self, SynapseError> {
        let template = template.trim();
        if template.is_empty() {
            return Err(SynapseError::UriTemplate("empty template".to_owned()));
        }
        if !template.starts_with('/') {
            return Err(SynapseError::UriTemplate(format!(
                "template must start with '/': {template}"
            )));
        }

        let mut segments = Vec::new();
        for raw in template.split('/').skip(1) {
            if raw.is_empty() {
                // Collapse duplicate and trailing slashes. A lone "/" has no
                // segments and matches only "/".
                continue;
            }
            if raw == "*" {
                segments.push(Segment::Wildcard);
                continue;
            }
            if let Some(name) = raw.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                let name = name.trim();
                if name.is_empty()
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
                {
                    return Err(SynapseError::UriTemplate(format!(
                        "invalid path parameter in {template}: {name}"
                    )));
                }
                segments.push(Segment::Param(name.to_owned()));
                continue;
            }
            if raw.contains('{') || raw.contains('}') {
                return Err(SynapseError::UriTemplate(format!(
                    "malformed parameter in {template}: {raw}"
                )));
            }
            segments.push(Segment::Literal(raw.to_owned()));
        }

        // A wildcard must be the final segment.
        if let Some(position) = segments
            .iter()
            .position(|segment| matches!(segment, Segment::Wildcard))
        {
            if position + 1 != segments.len() {
                return Err(SynapseError::UriTemplate(format!(
                    "wildcard must be the last segment in {template}"
                )));
            }
        }

        Ok(Self {
            source: template.to_owned(),
            segments,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    pub fn literal_segments(&self) -> usize {
        self.segments
            .iter()
            .filter(|segment| matches!(segment, Segment::Literal(_)))
            .count()
    }

    /// Higher is more specific. Artifact templates always outrank plain prefix
    /// routes with the same number of path characters.
    pub fn specificity(&self) -> usize {
        10_000 + self.literal_segments() * 100 + self.segment_count()
    }

    /// Match a request path and return captured path parameters.
    pub fn matches(&self, path: &str) -> Option<Vec<(String, String)>> {
        let path = path.trim_end_matches('/');
        let path = if path.is_empty() { "/" } else { path };
        let raw: Vec<&str> = path.split('/').skip(1).filter(|s| !s.is_empty()).collect();

        if self.segments.is_empty() {
            return if raw.is_empty() {
                Some(Vec::new())
            } else {
                None
            };
        }

        let has_wildcard = matches!(self.segments.last(), Some(Segment::Wildcard));
        if !has_wildcard && raw.len() != self.segments.len() {
            return None;
        }
        if has_wildcard && raw.len() < self.segments.len() - 1 {
            return None;
        }

        let mut captures = Vec::new();
        for (index, segment) in self.segments.iter().enumerate() {
            match segment {
                Segment::Literal(expected) => {
                    if raw.get(index).copied()? != expected {
                        return None;
                    }
                }
                Segment::Param(name) => {
                    let value = raw.get(index).copied()?;
                    if value.is_empty() {
                        return None;
                    }
                    captures.push((name.clone(), value.to_owned()));
                }
                Segment::Wildcard => {
                    let rest = raw[index..].join("/");
                    captures.push(("uri.var.wildcard".to_owned(), rest));
                }
            }
        }
        Some(captures)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_literals_and_parameters() {
        let pattern = PathPattern::parse("/pizzashack/1.0.0/menu/{id}").expect("pattern");
        let captures = pattern.matches("/pizzashack/1.0.0/menu/42").expect("match");
        assert_eq!(captures, vec![("id".to_owned(), "42".to_owned())]);
        assert!(pattern.matches("/pizzashack/1.0.0/menu").is_none());
        assert!(pattern.matches("/pizzashack/1.0.0/menu/42/extra").is_none());
    }

    #[test]
    fn supports_wildcard_tail() {
        let pattern = PathPattern::parse("/files/*").expect("pattern");
        let captures = pattern.matches("/files/a/b.txt").expect("match");
        assert_eq!(
            captures,
            vec![("uri.var.wildcard".to_owned(), "a/b.txt".to_owned())]
        );
    }

    #[test]
    fn rejects_malformed_templates() {
        assert!(PathPattern::parse("menu").is_err());
        assert!(PathPattern::parse("/menu/{}").is_err());
        assert!(PathPattern::parse("/menu/*/tail").is_err());
    }
}
