#![no_main]

use libfuzzer_sys::fuzz_target;

// Fuzz the production `${VAR}` / `${VAR:-default}` expander directly. The
// parser only reads the process environment, so the target is side-effect
// free and must never panic on arbitrary UTF-8 input.
fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };

    let _ = rusto2_common::env::expand_env(input);
});
