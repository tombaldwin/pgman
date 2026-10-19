//! Protocol-level tests. None of these touch the real config dir: they
//! stay on `initialize`, `ping`, `tools/list` and the error paths, and
//! the tool bodies' pure halves are tested in `tools.rs`.

use super::*;

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

fn server() -> Server {
    Server::new(Opts::default(), std::path::PathBuf::from("/nonexistent"))
}

#[test]
fn help_is_recognised_wherever_it_appears() {
    for a in [
        &["serve", "--help"][..],
        &["serve", "--read-only", "-h"],
        &["--help"],
        &["help"],
    ] {
        assert!(wants_help(&args(a)), "{a:?}");
    }
    assert!(!wants_help(&args(&["serve", "--read-only"])));
    assert!(!wants_help(&args(&[])));
}

#[test]
fn serve_is_required_and_flags_parse() {
    assert!(parse_args(&args(&[])).is_err());
    assert!(parse_args(&args(&["setup"])).is_err());
    assert_eq!(parse_args(&args(&["serve"])), Ok(Opts::default()));
    assert_eq!(
        parse_args(&args(&["serve", "--read-only", "--project-connections"])),
        Ok(Opts {
            read_only: true,
            project_connections: true
        })
    );
    assert!(parse_args(&args(&["serve", "--yes"]))
        .unwrap_err()
        .contains("unknown argument"));
}

#[test]
fn allow_writes_is_refused_rather_than_accepted_and_ignored() {
    for a in [
        "--allow-writes",
        "--allow-writes=apply_patches",
        "--allow-writes=",
    ] {
        let err = parse_args(&args(&["serve", a])).unwrap_err();
        assert!(err.contains("nothing to allow"), "{a}: {err}");
    }
}

#[tokio::test]
async fn initialize_records_the_client_and_negotiates_the_version() {
    let s = server();
    let resp = s
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2099-01-01",
                "clientInfo": {"name": "claude-code"},
                "capabilities": {"elicitation": {}}
            }
        }))
        .await
        .unwrap();
    assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(resp["result"]["serverInfo"]["name"], "pgman");
    let text = resp["result"]["instructions"].as_str().unwrap();
    assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
    assert!(text.contains("Read-only"), "{text}");
    assert_eq!(s.client_info(), ("claude-code".to_string(), true));
}

#[tokio::test]
async fn elicitation_false_is_not_read_as_support() {
    let s = server();
    s.handle_request(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"capabilities": {"elicitation": false}}
    }))
    .await;
    assert!(!s.client_info().1);
}

#[tokio::test]
async fn notifications_are_never_answered() {
    let s = server();
    for req in [
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": null, "method": "tools/call"}),
    ] {
        assert_eq!(s.handle_request(&req).await, None, "{req}");
    }
}

#[tokio::test]
async fn unknown_methods_and_tools_get_the_right_error_codes() {
    let s = server();
    let r = s
        .handle_request(&json!({"jsonrpc": "2.0", "id": 2, "method": "resources/list"}))
        .await
        .unwrap();
    assert_eq!(r["error"]["code"], -32601);
    let r = s
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "apply_patches", "arguments": {}}
        }))
        .await
        .unwrap();
    assert_eq!(r["error"]["code"], -32602);
}

#[tokio::test]
async fn a_tool_error_is_a_result_with_is_error_not_a_protocol_error() {
    let s = server();
    let r = s
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "query", "arguments": {"sql": "SELECT 1"}}
        }))
        .await
        .unwrap();
    assert_eq!(r["result"]["isError"], true);
    let text = r["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("'connection'"), "{text}");
}

async fn frames(input: &[u8], max: usize) -> Vec<Frame> {
    let mut r = tokio::io::BufReader::with_capacity(4, input);
    let mut out = Vec::new();
    loop {
        let f = next_frame(&mut r, max).await.unwrap();
        let eof = f == Frame::Eof;
        out.push(f);
        if eof {
            return out;
        }
    }
}

#[tokio::test]
async fn frames_split_on_newlines_and_an_unterminated_tail_still_counts() {
    assert_eq!(
        frames(b"ab\ncdefgh\nij", 100).await,
        [
            Frame::Line("ab".into()),
            Frame::Line("cdefgh".into()),
            Frame::Line("ij".into()),
            Frame::Eof
        ]
    );
}

#[tokio::test]
async fn an_over_long_frame_is_skipped_and_the_stream_stays_in_step() {
    // Capacity 4 makes the long line arrive over several fills.
    assert_eq!(
        frames(b"0123456789\nok\n", 5).await,
        [Frame::Bad, Frame::Line("ok".into()), Frame::Eof]
    );
    assert_eq!(
        frames(b"12345\n", 5).await,
        [Frame::Line("12345".into()), Frame::Eof]
    );
    assert_eq!(frames(b"0123456789", 5).await, [Frame::Bad, Frame::Eof]);
    assert_eq!(
        frames(b"\xff\xfe\nok\n", 5).await,
        [Frame::Bad, Frame::Line("ok".into()), Frame::Eof]
    );
}

#[tokio::test]
async fn serve_answers_each_request_and_exits_when_input_closes() {
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n",
        "not json\n",
        "[1,2]\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
    );
    let (client_end, server_end) = tokio::io::duplex(1 << 20);
    serve(
        Arc::new(server()),
        tokio::io::BufReader::new(input.as_bytes()),
        server_end,
    )
    .await;
    use tokio::io::AsyncReadExt;
    let mut out = String::new();
    let mut client_end = client_end;
    client_end.read_to_string(&mut out).await.unwrap();
    let frames: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(frames.len(), 4, "{out}");
    assert_eq!(frames[0]["id"], 1);
    assert_eq!(frames[1]["error"]["code"], -32700);
    assert_eq!(frames[2]["error"]["code"], -32600);
    assert_eq!(frames[3]["id"], 2);
    assert_eq!(frames[3]["result"]["tools"].as_array().unwrap().len(), 6);
}
