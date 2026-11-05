use super::*;

#[test]
fn durable_and_live_timestamps_sort_in_the_same_utc_format() {
    let old = durable_timestamp("2026-10-09 12:00:00");
    let newer = timestamp(
        chrono::NaiveDateTime::parse_from_str("2026-10-09 12:00:00", "%Y-%m-%d %H:%M:%S")
            .unwrap()
            .and_utc()
            .timestamp() as f64
            + 0.9,
    );
    assert!(newer > old);
    assert_eq!(old, "2026-10-09T12:00:00.000Z");
    assert_eq!(newer, "2026-10-09T12:00:00.900Z");
    // Not a database time: kept as it is.
    assert_eq!(
        durable_timestamp("2026-10-09T12:00:00Z"),
        "2026-10-09T12:00:00Z"
    );
}

#[test]
fn digests_are_lowercase_hex_and_clocks_agree() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let secs = now_ts();
    let nanos = unix_nanos();
    assert!((nanos as f64 / 1e9 - secs).abs() < 5.0);
    assert_eq!(step_status::of_event(true, false), "cached");
    assert_eq!(step_status::of_event(false, true), "success");
    assert_eq!(step_status::of_event(false, false), "failed");
}
