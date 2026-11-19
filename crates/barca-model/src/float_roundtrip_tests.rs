//! A float printed as JSON and read back is the same float. A client that reads a server's
//! answer into a typed value and prints it again (`barca history --server`) must print what the
//! server sent: without serde_json's `float_roundtrip`, parsing is fast and can be one unit in
//! the last place off, so the CLI and a server disagreed on a run's `elapsed_seconds` now and
//! then (#1056).

#[test]
fn a_float_read_back_from_json_is_the_same_float() {
    // A fixed sequence of durations of the size barca records (microseconds to hours).
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut off = Vec::new();
    for _ in 0..200_000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let value = (state >> 11) as f64 / (1u64 << 53) as f64 * 7200.0;
        let text = serde_json::to_string(&value).unwrap();
        let back: f64 = serde_json::from_str(&text).unwrap();
        if back.to_bits() != value.to_bits() {
            off.push(text);
        }
    }
    assert!(
        off.is_empty(),
        "{} of 200000 changed, e.g. {}",
        off.len(),
        off[0]
    );
}
