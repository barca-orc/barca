use super::SseParser;

#[test]
fn events_are_split_across_chunks_and_comments_are_skipped() {
    let mut sse = SseParser::default();
    assert!(
        sse.push(b": keep-alive\n\nid: 0\ndata: {\"type\":")
            .unwrap()
            .is_empty()
    );
    let got = sse.push("\"line\",\"line\":\"é\"}\n\nid: 1\r\ndata: {}\r\n\r\n".as_bytes());
    assert_eq!(
        got.unwrap()
            .into_iter()
            .map(|frame| frame.data)
            .collect::<Vec<_>>(),
        vec![r#"{"type":"line","line":"é"}"#, "{}"]
    );
    // A multibyte character split between chunks.
    let bytes = "data: \u{e9}\n\n".as_bytes();
    assert!(sse.push(&bytes[..7]).unwrap().is_empty());
    assert_eq!(
        sse.push(&bytes[7..])
            .unwrap()
            .into_iter()
            .map(|frame| frame.data)
            .collect::<Vec<_>>(),
        vec!["\u{e9}"]
    );
}
