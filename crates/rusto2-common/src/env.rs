use std::fmt;

/// Expand `${VAR}` and `${VAR:-default}` placeholders in configuration files.
///
/// Secrets should be injected through the environment rather than committed.
/// Missing variables without a default are a hard error so a typo cannot
/// silently disable authentication.
pub fn expand_env(input: &str) -> Result<String, EnvExpandError> {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Err(EnvExpandError::Unclosed(rest[start..].to_owned()));
        };

        let expression = &after[..end];
        let (name, default) = match expression.split_once(":-") {
            Some((name, default)) => (name, Some(default)),
            None => (expression, None),
        };

        if !is_valid_name(name) {
            return Err(EnvExpandError::InvalidName(expression.to_owned()));
        }

        match std::env::var(name) {
            Ok(value) => output.push_str(&value),
            Err(_) => match default {
                Some(value) => output.push_str(value),
                None => return Err(EnvExpandError::Missing(name.to_owned())),
            },
        }

        rest = &after[end + 1..];
    }

    output.push_str(rest);
    Ok(output)
}

fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvExpandError {
    Unclosed(String),
    InvalidName(String),
    Missing(String),
}

impl fmt::Display for EnvExpandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unclosed(fragment) => {
                write!(formatter, "unclosed environment placeholder: {fragment}")
            }
            Self::InvalidName(expression) => {
                write!(formatter, "invalid environment variable name: {expression}")
            }
            Self::Missing(name) => write!(
                formatter,
                "environment variable {name} is not set and has no default"
            ),
        }
    }
}

impl std::error::Error for EnvExpandError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_defaults_and_plain_text() {
        // Use a variable that is extremely unlikely to exist.
        let expanded =
            expand_env("listen: 0.0.0.0:8080\nsecret: ${RUSTO2_TEST_UNSET_SECRET:-demo}")
                .expect("expansion should succeed");
        assert!(expanded.contains("secret: demo"));
        assert!(expanded.contains("listen: 0.0.0.0:8080"));
    }

    #[test]
    fn fails_on_missing_without_default() {
        let error = expand_env("${RUSTO2_TEST_DEFINITELY_UNSET}").expect_err("must fail");
        assert!(matches!(error, EnvExpandError::Missing(_)));
    }

    #[test]
    fn fails_on_unclosed_placeholder() {
        let error = expand_env("${OOPS").expect_err("must fail");
        assert!(matches!(error, EnvExpandError::Unclosed(_)));
    }

    #[test]
    fn rejects_shell_injection_in_names() {
        let error = expand_env("${FOO;rm -rf /}").expect_err("must fail");
        assert!(matches!(error, EnvExpandError::InvalidName(_)));
    }
}
