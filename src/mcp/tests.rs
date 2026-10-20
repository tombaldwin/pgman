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
    assert!(
        text.contains("apply_patches is the one tool that writes"),
        "{text}"
    );
    assert!(text.contains("environments_save appends"), "{text}");
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
            "params": {"name": "drop_database", "arguments": {}}
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
    assert_eq!(frames[3]["result"]["tools"].as_array().unwrap().len(), 11);
}

/// A question to the operator goes out as `elicitation/create`, and the
/// client's response — matched by id — comes back to the asker; closing
/// the stream ends a wait instead of leaving it hanging.
#[tokio::test]
async fn an_elicitation_goes_out_and_its_answer_comes_back() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let s = Arc::new(server());
    assert!(s
        .elicit("q", json!({}))
        .await
        .unwrap_err()
        .contains("declared no elicitation"));
    let (mut to_server, server_in) = tokio::io::duplex(1 << 16);
    let (server_out, from_server) = tokio::io::duplex(1 << 16);
    let serving = tokio::spawn(serve(
        s.clone(),
        tokio::io::BufReader::new(server_in),
        server_out,
    ));
    let mut lines = tokio::io::BufReader::new(from_server).lines();
    to_server
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"capabilities\":{\"elicitation\":{}}}}\n")
        .await
        .unwrap();
    lines.next_line().await.unwrap().unwrap();

    let asker = {
        let s = s.clone();
        tokio::spawn(async move {
            s.elicit("Apply 2 patches?", json!({"type": "object"}))
                .await
        })
    };
    let req: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(req["method"], "elicitation/create");
    assert_eq!(req["params"]["message"], "Apply 2 patches?");
    let id = req["id"].as_str().unwrap().to_string();
    // A response to an id nobody asked about is dropped, not misdelivered.
    to_server
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"pgman-elicit-999\",\"result\":{\"action\":\"accept\"}}\n")
        .await
        .unwrap();
    let answer = json!({"jsonrpc": "2.0", "id": id, "result": {"action": "decline"}});
    to_server
        .write_all(format!("{answer}\n").as_bytes())
        .await
        .unwrap();
    assert_eq!(asker.await.unwrap().unwrap(), json!({"action": "decline"}));

    let asker = {
        let s = s.clone();
        tokio::spawn(async move { s.elicit("again?", json!({})).await })
    };
    lines.next_line().await.unwrap().unwrap();
    drop(to_server);
    let err = tokio::time::timeout(std::time::Duration::from_secs(5), asker)
        .await
        .expect("a closed stream ends the wait")
        .unwrap()
        .unwrap_err();
    assert_eq!(err, "the client went away");
    serving.await.unwrap();
}

#[test]
fn only_an_accepted_matching_answer_confirms_a_patch_plan() {
    use crate::inventory::patching::PatchPlan;
    let mut plan = PatchPlan {
        environment: "production".into(),
        server: "production".into(),
        database: "ledger".into(),
        protected: true,
        applied_max: Some(251),
        patches: vec![crate::inventory::patching::PlannedPatch {
            file: crate::query::patches::PatchFile {
                file: "252-x.sql".into(),
                filename_number: Some(252),
                version_id: Some(252),
            },
            text: String::new(),
            statements: vec!["ALTER TABLE orders\n  DROP COLUMN notes".into()],
            outside: None,
        }],
        stopped_at: Some(("253-idx.sql".into(), "CONCURRENTLY".into())),
        dir: "/src/patches".into(),
        recorded: (1..=251).collect(),
        below_recorded: Vec::new(),
        outside_transaction: false,
        outside_would_help: false,
    };
    let (message, schema) = tools::confirm_request(&plan);
    assert!(message.contains("recorded up to 251"), "{message}");
    assert!(
        message.contains("Not included: 253-idx.sql (CONCURRENTLY)"),
        "{message}"
    );
    assert!(message.contains("type its name"), "{message}");
    assert!(
        message.contains("files in /src/patches") && message.contains("\n  DROP COLUMN notes;"),
        "every line that will run is shown: {message}"
    );
    plan.below_recorded = vec!["7-old.sql".into()];
    assert!(
        tools::confirm_request(&plan)
            .0
            .contains("WARNING, not included: 7-old.sql — pending but numbered below"),
        "a skipped gap is put to the operator"
    );
    let result: Value =
        serde_json::from_str(&tools::render_apply(&plan, None, "applied 0 patch(es)")).unwrap();
    assert_eq!(result["below_recorded"], json!(["7-old.sql"]));
    let report = crate::inventory::patching::ApplyReport {
        recorded_after: Some(252),
        ..Default::default()
    };
    let result: Value =
        serde_json::from_str(&tools::render_apply(&plan, Some(&report), "")).unwrap();
    assert_eq!(result["recorded_after"], json!(252));
    plan.patches[0].statements = vec!["x".repeat(7000)];
    assert!(tools::confirm_request(&plan)
        .0
        .contains("read the files before confirming"));
    assert_eq!(schema["required"], json!(["environment"]));
    let ok = json!({"action": "accept", "content": {"environment": "production"}});
    assert_eq!(tools::confirmed(&plan, &ok), Ok(()));
    for bad in [
        json!({"action": "accept", "content": {"environment": "Production"}}),
        json!({"action": "accept", "content": {"apply": true}}),
        json!({"action": "decline"}),
        json!({"action": "cancel", "content": {"environment": "production"}}),
        json!({}),
    ] {
        assert!(tools::confirmed(&plan, &bad).is_err(), "{bad}");
    }
    plan.protected = false;
    let (_, schema) = tools::confirm_request(&plan);
    assert_eq!(schema["required"], json!(["apply"]));
    assert_eq!(
        tools::confirmed(
            &plan,
            &json!({"action": "accept", "content": {"apply": true}})
        ),
        Ok(())
    );
    assert!(tools::confirmed(
        &plan,
        &json!({"action": "accept", "content": {"apply": false}})
    )
    .is_err());
}

#[tokio::test]
async fn apply_patches_is_refused_read_only_or_without_elicitation() {
    let call = json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": {"name": "apply_patches", "arguments": {"environment": "e", "server": "s", "database": "d"}}
    });
    let ro = Server::new(
        Opts {
            read_only: true,
            ..Opts::default()
        },
        std::path::PathBuf::from("/nonexistent"),
    );
    let r = ro.handle_request(&call).await.unwrap();
    assert_eq!(r["result"]["isError"], true);
    assert!(r["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("--read-only"));
    let r = server().handle_request(&call).await.unwrap();
    assert!(r["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("needs a client that supports elicitation"));
}
