#![no_main]

use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine as _;
use libfuzzer_sys::fuzz_target;

// Exercise the JWT header parser and the base64/JSON handling of untrusted
// token segments. Arbitrary input must only produce `Err` values, never a
// panic or an out-of-bounds access.
fuzz_target!(|data: &[u8]| {
    let Ok(token) = std::str::from_utf8(data) else {
        // JWT compact serialization is ASCII; non-UTF-8 cannot reach parsing.
        return;
    };

    // The public entry point used by the gateway before signature checks.
    let _ = jsonwebtoken::decode_header(token);

    // Callers often split the compact serialization first; do the same and
    // feed every segment through the decoders.
    for segment in token.split('.') {
        if let Ok(decoded) = URL_SAFE_NO_PAD.decode(segment) {
            let _ = serde_json::from_slice::<serde_json::Value>(&decoded);
        }
        if let Ok(decoded) = URL_SAFE.decode(segment) {
            let _ = serde_json::from_slice::<serde_json::Value>(&decoded);
        }

        // A segment that is itself used as a token header (for example when a
        // caller re-serialises a decoded header) must be safe as well.
        let _ = jsonwebtoken::decode_header(segment);
    }
});
