use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const EPOCH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
struct Reply {
    status: u16,
    body: String,
    sse: bool,
}
impl Reply {
    fn json(body: Value) -> Self {
        Self {
            status: 200,
            body: body.to_string(),
            sse: false,
        }
    }
    fn sse(body: String) -> Self {
        Self {
            status: 200,
            body,
            sse: true,
        }
    }
    fn status(status: u16) -> Self {
        Self {
            status,
            body: json!({"kind":"infra","code":3,"error":"refused","remediation":null}).to_string(),
            sse: false,
        }
    }
}
struct Mock {
    server: Server,
    calls: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn mock(handler: impl Fn(&str) -> Reply + Send + 'static) -> Mock {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                if socket.read(&mut byte).await.unwrap() == 0 {
                    break;
                }
                bytes.push(byte[0]);
                assert!(bytes.len() < 32_768);
            }
            let request = String::from_utf8(bytes).unwrap();
            recorded.lock().unwrap().push(request.clone());
            let reply = handler(&request);
            let mime = if reply.sse {
                "text/event-stream"
            } else {
                "application/json"
            };
            let head = format!(
                "HTTP/1.1 {} OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                reply.status,
                reply.body.len()
            );
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(reply.body.as_bytes()).await.unwrap();
        }
    });
    let http = transport::Http::new(&url, None, REQUEST_TIMEOUT).unwrap();
    Mock {
        server: Server {
            url: url.clone(),
            shown_url: url,
            http,
            source: None,
            implicit: false,
        },
        calls,
        task,
    }
}
fn health(resume: bool) -> Reply {
    Reply::json(if resume {
        json!({"started_at":"one","capabilities":{"event_resume":1}})
    } else {
        json!({"started_at":"one"})
    })
}
fn frame(id: &str, event: Value) -> String {
    format!("id: {id}\r\ndata: {event}\r\n\r\n")
}
fn started(id: &str) -> String {
    frame(id, json!({"type":"run_started","run_id":"r"}))
}
fn terminal(id: &str) -> String {
    frame(id, json!({"type":"run_finished","run_id":"r","ok":true}))
}

#[tokio::test]
async fn new_cursors_and_legacy_count_reconnect_both_render_each_event_once() {
    for capable in [true, false] {
        let opens = AtomicUsize::new(0);
        let fixture = mock(move |request| {
            if request.starts_with("GET /health ") {
                return health(capable);
            }
            assert!(request.starts_with("GET /runs/r/events "));
            let open = opens.fetch_add(1, Ordering::SeqCst);
            let first = if capable {
                format!("{EPOCH}:1")
            } else {
                "0".into()
            };
            if open == 0 {
                return Reply::sse(started(&first) + "id: unfinished\ndata: {\"type\":");
            }
            assert_eq!(request.to_lowercase().contains("last-event-id:"), capable);
            if capable {
                assert!(request.contains(&format!("{EPOCH}:1")));
            }
            let prefix = if capable { String::new() } else { started("0") };
            let next = if capable {
                format!("{EPOCH}:2")
            } else {
                "1".into()
            };
            let end = if capable {
                format!("{EPOCH}:3")
            } else {
                "2".into()
            };
            Reply::sse(
                prefix
                    + &frame(
                        &next,
                        json!({"type":"log","node_id":"p.py:a","line":"once","stream":"stdout"}),
                    )
                    + &terminal(&end),
            )
        })
        .await;
        let mut follow = Follow::new(&fixture.server, "r", Duration::from_secs(5)).await;
        assert!(follow.open().await.unwrap());
        assert!(matches!(
            follow.next().await.unwrap(),
            Followed::Event(RunEvent::RunStarted { .. })
        ));
        assert!(
            follow.last_cursor.is_none(),
            "delivery is not an acknowledgement"
        );
        assert!(
            matches!(follow.next().await.unwrap(), Followed::Event(RunEvent::Log { line, .. }) if line == "once")
        );
        assert!(matches!(
            follow.next().await.unwrap(),
            Followed::Event(RunEvent::RunFinished { .. })
        ));
        assert_eq!(
            fixture
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|call| call.starts_with("GET /runs/r/events "))
                .count(),
            2
        );
    }
}

#[tokio::test]
async fn an_expired_stream_polls_healthy_runs_beyond_the_outage_window_without_posting() {
    let polls = AtomicUsize::new(0);
    let fixture = mock(move |request| {
        if request.starts_with("GET /health ") {
            return health(true);
        }
        if request.starts_with("GET /runs/r/events ") {
            return Reply::status(410);
        }
        assert!(request.starts_with("GET /runs/r "));
        Reply::json(if polls.fetch_add(1, Ordering::SeqCst) == 0 {
            json!({"status":"running"})
        } else {
            json!({"status":"success","outcome":{"status":"success"}})
        })
    })
    .await;
    let mut follow = Follow::new(&fixture.server, "r", Duration::from_millis(20)).await;
    assert!(follow.open().await.unwrap());
    let start = std::time::Instant::now();
    assert!(
        matches!(follow.next().await.unwrap(), Followed::Ended(body) if body["status"] == "success")
    );
    assert!(start.elapsed() >= Duration::from_secs(2));
    assert!(
        fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|call| call.starts_with("GET "))
    );
}

#[tokio::test]
async fn complete_log_follow_reports_an_unfillable_gap() {
    let fixture = mock(|request| {
        if request.starts_with("GET /health ") {
            health(true)
        } else {
            Reply::status(410)
        }
    })
    .await;
    let mut follow = Follow::new(&fixture.server, "r", Duration::from_secs(60)).await;
    follow.complete_logs = true;
    let error = follow.open().await.unwrap_err();
    assert!(error.error.contains("unfillable log gap"));
    assert_eq!(fixture.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn auth_refusal_during_reconnect_is_terminal_even_without_an_auth_reason() {
    let probes = AtomicUsize::new(0);
    let fixture = mock(move |request| {
        if request.starts_with("GET /health ") {
            return if probes.fetch_add(1, Ordering::SeqCst) == 0 {
                health(true)
            } else {
                Reply::status(403)
            };
        }
        Reply::sse(started(&format!("{EPOCH}:1")))
    })
    .await;
    let mut follow = Follow::new(&fixture.server, "r", Duration::from_secs(60)).await;
    assert!(follow.open().await.unwrap());
    let _ = follow.next().await.unwrap();
    let began = std::time::Instant::now();
    assert!(follow.next().await.is_err());
    assert!(began.elapsed() < Duration::from_secs(3));
    assert_eq!(fixture.calls.lock().unwrap().len(), 3);
}

#[test]
fn bytewise_utf8_cr_and_crlf_frames_keep_ids_and_multiline_data() {
    let data = format!(
        "\u{feff}: comment\rid: {EPOCH}:1\rdata: café\rdata: second\r\rid: {EPOCH}:2\r\ndata: done\r\n\r\n"
    );
    let mut parser = SseParser::default();
    let mut frames = Vec::new();
    for byte in data.bytes() {
        frames.extend(parser.push(&[byte]).unwrap());
    }
    assert_eq!(
        frames,
        vec![
            SseFrame {
                id: Some(format!("{EPOCH}:1")),
                data: "café\nsecond".into()
            },
            SseFrame {
                id: Some(format!("{EPOCH}:2")),
                data: "done".into()
            }
        ]
    );
    assert!(SseParser::default().push(&[255, b'\n']).is_err());
}

#[tokio::test]
async fn epoch_mismatch_follows_a_new_attempt_of_the_same_run_without_resubmission() {
    let opens = AtomicUsize::new(0);
    let fixture = mock(move |request| {
        if request.starts_with("GET /health ") {
            return health(true);
        }
        if request.starts_with("GET /runs/r ") {
            return Reply::json(json!({"status":"running"}));
        }
        assert!(request.starts_with("GET /runs/r/events "));
        match opens.fetch_add(1, Ordering::SeqCst) {
            0 => Reply::sse(started(&format!("{EPOCH}:1"))),
            1 => {
                assert!(request.to_lowercase().contains("last-event-id:"));
                Reply::status(409)
            }
            2 => {
                assert!(!request.to_lowercase().contains("last-event-id:"));
                Reply::sse(
                    started("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:1")
                        + &terminal("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:2"),
                )
            }
            _ => panic!("unexpected stream request"),
        }
    })
    .await;
    let mut follow = Follow::new(&fixture.server, "r", Duration::from_secs(10)).await;
    assert!(follow.open().await.unwrap());
    assert!(matches!(
        follow.next().await.unwrap(),
        Followed::Event(RunEvent::RunStarted { .. })
    ));
    assert!(matches!(
        follow.next().await.unwrap(),
        Followed::Event(RunEvent::RunStarted { .. })
    ));
    assert_eq!(follow.restarts, 1);
    assert!(matches!(
        follow.next().await.unwrap(),
        Followed::Event(RunEvent::RunFinished { .. })
    ));
    assert!(
        fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|call| call.starts_with("GET "))
    );
}

#[tokio::test]
async fn missing_initial_stream_does_not_mistake_a_pending_run_for_a_terminal_outcome() {
    for status in [204, 404] {
        let opens = AtomicUsize::new(0);
        let fixture = mock(move |request| {
            if request.starts_with("GET /health ") {
                return health(true);
            }
            if request.starts_with("GET /runs/r ") {
                return Reply::json(json!({"status":"pending"}));
            }
            assert!(request.starts_with("GET /runs/r/events "));
            if opens.fetch_add(1, Ordering::SeqCst) == 0 {
                Reply::status(status)
            } else {
                Reply::sse(terminal(&format!("{EPOCH}:1")))
            }
        })
        .await;
        let mut follow = Follow::new(&fixture.server, "r", Duration::from_secs(5)).await;
        assert!(follow.open().await.unwrap());
        assert!(matches!(
            follow.next().await.unwrap(),
            Followed::Event(RunEvent::RunFinished { .. })
        ));
    }
}
