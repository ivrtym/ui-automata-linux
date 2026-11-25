use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn send_msg(stdin: &mut impl Write, val: &Value) {
    let line = serde_json::to_string(val).unwrap();
    writeln!(stdin, "{line}").unwrap();
    stdin.flush().unwrap();
}

fn recv_msg(reader: &mut BufReader<impl std::io::Read>) -> Value {
    let mut line = String::new();
    let n = reader.read_line(&mut line).unwrap();
    assert!(n > 0, "unexpected EOF from automata-agent --stdio");
    serde_json::from_str(line.trim()).unwrap_or_else(|e| {
        panic!("stdout must be clean JSON-RPC; got invalid JSON ({e}): {line:?}")
    })
}

#[test]
fn stdio_mcp_protocol_handshake_tools_resources_and_calls() {
    let bin = env!("CARGO_BIN_EXE_automata-agent");
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workflows_dir = manifest_dir.parent().unwrap().join("workflows");

    let mut child = Command::new(bin)
        .args(["--stdio", "--connection-mode", "oneshot", "--workflows-dir"])
        .arg(&workflows_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn automata-agent --stdio");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    // 1. initialize
    send_msg(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": { "name": "test-client", "version": "0.1.0" }
            }
        }),
    );
    let init_resp = recv_msg(&mut reader);
    assert_eq!(init_resp["id"], 1);
    assert_eq!(init_resp["result"]["serverInfo"]["name"], "ui-automata");
    assert!(init_resp["result"]["capabilities"]["tools"].is_object());
    assert!(init_resp["result"]["capabilities"]["resources"].is_object());

    // 2. initialized notification
    send_msg(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    );

    // 3. tools/list == 13 tools
    send_msg(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }),
    );
    let tools_resp = recv_msg(&mut reader);
    let tools = tools_resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 13);

    // 4. resources/list + resources/read
    send_msg(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "resources/list",
            "params": {}
        }),
    );
    let res_resp = recv_msg(&mut reader);
    let resources = res_resp["result"]["resources"].as_array().unwrap();
    assert!(
        resources
            .iter()
            .any(|r| r["uri"] == "automata://linux/AGENT.md"),
        "expected linux/AGENT.md in resources: {resources:?}"
    );

    send_msg(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "resources/read",
            "params": { "uri": "automata://linux/AGENT.md" }
        }),
    );
    let read_res_resp = recv_msg(&mut reader);
    let text = read_res_resp["result"]["contents"][0]["text"]
        .as_str()
        .unwrap();
    assert!(text.contains("AT-SPI2"));

    // 5. tools/call system ping & workflow lint
    send_msg(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "system",
                "arguments": { "action": "ping" }
            }
        }),
    );
    let ping_resp = recv_msg(&mut reader);
    assert_eq!(ping_resp["result"]["isError"], false);
    assert_eq!(ping_resp["result"]["structuredContent"]["pong"], true);

    send_msg(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {
                "name": "workflow",
                "arguments": {
                    "action": "lint",
                    "yaml": "name: t\nphases: []\n"
                }
            }
        }),
    );
    let lint_resp = recv_msg(&mut reader);
    assert_eq!(lint_resp["result"]["structuredContent"]["valid"], true);

    // Cleanly close stdin to terminate server
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "automata-agent exited with {status}");
}

#[test]
fn http_mcp_endpoint_responds_to_initialize() {
    use std::io::{BufRead as _, BufReader, Read as _};
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    let probe = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    let bin = env!("CARGO_BIN_EXE_automata-agent");
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workflows_dir = manifest_dir.parent().unwrap().join("workflows");

    let mut child = Command::new(bin)
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--connection-mode",
            "oneshot",
            "--auth-token",
            "m0-secret",
            "--workflows-dir",
        ])
        .arg(&workflows_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn automata-agent HTTP server");

    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        line.clear();
        if stderr.read_line(&mut line).unwrap_or(0) == 0 {
            panic!("HTTP server exited before listening");
        }
        if line.contains("listening on http://") {
            break;
        }
        assert!(Instant::now() < deadline, "HTTP server timed out before listening");
    }

    let addr = format!("127.0.0.1:{port}");
    let body = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "http-test", "version": "0.1.0" }
        }
    }))
    .unwrap();

    let send_raw = |raw_req: String| -> String {
        let mut s = TcpStream::connect(&addr).unwrap();
        s.write_all(raw_req.as_bytes()).unwrap();
        s.flush().unwrap();
        let mut resp = String::new();
        s.read_to_string(&mut resp).unwrap();
        resp
    };

    // 1. Browser Origin header must be rejected (403 Forbidden).
    let resp_origin = send_raw(format!(
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nOrigin: https://evil.example\r\nAuthorization: Bearer m0-secret\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    ));
    assert!(resp_origin.starts_with("HTTP/1.1 403"), "expected 403 for Origin, got: {resp_origin}");

    // 2. Unknown Host header must be rejected (403 Forbidden).
    let resp_host = send_raw(format!(
        "POST /mcp HTTP/1.1\r\nHost: evil.example:3742\r\nAuthorization: Bearer m0-secret\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    ));
    assert!(resp_host.starts_with("HTTP/1.1 403"), "expected 403 for Host, got: {resp_host}");

    // 3. Missing Bearer token must be rejected (401 Unauthorized).
    let resp_unauth = send_raw(format!(
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    ));
    assert!(resp_unauth.starts_with("HTTP/1.1 401"), "expected 401 without token, got: {resp_unauth}");

    // 4. Valid loopback Host + Bearer token without Origin succeeds (200 OK).
    let resp_ok = send_raw(format!(
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer m0-secret\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    ));
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        resp_ok.starts_with("HTTP/1.1 200 OK"),
        "expected HTTP 200 OK from /mcp, got: {resp_ok}"
    );
    assert!(
        resp_ok.contains("\"ui-automata\""),
        "expected serverInfo name ui-automata in HTTP /mcp response, got: {resp_ok}"
    );
}
