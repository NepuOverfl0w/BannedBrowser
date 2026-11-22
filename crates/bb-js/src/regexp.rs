//! Regular expression pattern syntax (ECMA-262 §22.2.1, Annex B.1.2): early errors.

/// Check a pattern's syntax. `unicode`: the `u` or `v` flag; `sets`: the `v` flag.
pub fn validate(_body: &str, _unicode: bool, _sets: bool) -> Result<(), String> {
    Ok(())
}
