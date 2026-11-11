//! Property tests for `${VAR}` / `${VAR:-default}` expansion.
//!
//! The tests never mutate the process environment: they only reference names
//! with a `RUSTO2_PROPTEST_UNSET_` prefix that is not expected to exist.

use proptest::prelude::*;
use proptest::string::string_regex;
use rusto2_common::env::{expand_env, EnvExpandError};

/// Arbitrary configuration text that does not open a placeholder.
fn plain_text() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..64)
        .prop_map(|chars| chars.into_iter().collect::<String>())
        .prop_filter(
            "plain text must not contain a placeholder opener",
            |text: &String| !text.contains("${"),
        )
}

/// A syntactically valid name for a variable that is extremely unlikely to be
/// present in the environment running the tests.
fn unset_name() -> impl Strategy<Value = String> {
    string_regex("[A-Z0-9_]{0,16}")
        .expect("valid regex")
        .prop_map(|suffix| format!("RUSTO2_PROPTEST_UNSET_{suffix}"))
}

/// Default values may contain almost anything except the placeholder closing
/// brace (which would terminate the expression early) and `$`.
fn default_text() -> impl Strategy<Value = String> {
    string_regex("[A-Za-z0-9 _./:-]{0,32}").expect("valid regex")
}

const INVALID_NAME_CHARS: &[char] = &[
    ' ', ';', '-', '.', '/', '!', '@', '#', '%', '*', '+', '=', '?', ',', '~', '|', '\\', '\'',
    '"', '[', ']', '(', ')', '<', '>', '&', '^',
];

/// A name that is guaranteed to contain at least one character outside
/// `[A-Za-z0-9_]`, and none of `}`, `$`, `{` or `:` that would change how the
/// placeholder is tokenised.
fn invalid_name() -> impl Strategy<Value = String> {
    (
        string_regex("[A-Za-z0-9_]{0,8}").expect("valid regex"),
        prop::sample::select(INVALID_NAME_CHARS.to_vec()),
        string_regex("[A-Za-z0-9_]{0,8}").expect("valid regex"),
    )
        .prop_map(|(prefix, invalid, suffix)| format!("{prefix}{invalid}{suffix}"))
}

proptest! {
    /// Text without a `${` sequence is copied verbatim.
    #[test]
    fn plain_text_is_unchanged(text in plain_text()) {
        let expanded = expand_env(&text).expect("plain text cannot fail to expand");
        prop_assert_eq!(expanded, text);
    }

    /// `${NAME:-default}` uses the default when the variable is unset, and
    /// preserves the surrounding text.
    #[test]
    fn unset_variables_use_the_default(
        name in unset_name(),
        default in default_text(),
        before in plain_text(),
        after in plain_text(),
    ) {
        prop_assume!(std::env::var(&name).is_err());

        let input = format!("{before}${{{name}:-{default}}}{after}");
        let expanded = expand_env(&input).expect("valid placeholder must expand");

        prop_assert_eq!(expanded, format!("{before}{default}{after}"));
    }

    /// Empty names and names with characters outside `[A-Za-z0-9_]` are
    /// rejected instead of being passed to the shell or the environment.
    #[test]
    fn invalid_names_are_rejected(name in invalid_name()) {
        let input = format!("${{{name}}}");
        let error = expand_env(&input).expect_err("invalid name must be rejected");

        prop_assert!(
            matches!(&error, EnvExpandError::InvalidName(_)),
            "unexpected error for {input:?}: {error:?}",
        );
    }

    /// A `${` without a closing brace is rejected rather than truncating the
    /// configuration silently.
    #[test]
    fn unclosed_placeholders_are_rejected(
        before in plain_text(),
        name in unset_name(),
    ) {
        let input = format!("{before}${{{name}");
        let error = expand_env(&input).expect_err("unclosed placeholder must be rejected");

        prop_assert!(
            matches!(&error, EnvExpandError::Unclosed(_)),
            "unexpected error for {input:?}: {error:?}",
        );
    }
}
