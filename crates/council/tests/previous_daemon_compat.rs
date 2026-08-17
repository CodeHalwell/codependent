//! Current client compatibility with a frozen v0.9 daemon wire fixture.

use codypendent_council::connection::Connection;
use codypendent_protocol::Payload;
use serde_json::{json, Value};
use std::time::Duration;
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

async fn read_json_frame(stream: &mut UnixStream) -> Value {
    let length = stream.read_u32().await.expect("read frame length");
    let mut bytes = vec![0; length as usize];
    stream
        .read_exact(&mut bytes)
        .await
        .expect("read frame body");
    serde_json::from_slice(&bytes).expect("parse frame")
}

async fn write_json_frame(stream: &mut UnixStream, value: &Value) {
    let bytes = serde_json::to_vec(value).expect("serialize frame");
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await
        .expect("write frame length");
    stream.write_all(&bytes).await.expect("write frame body");
    stream.flush().await.expect("flush frame");
}

#[tokio::test]
async fn current_client_handshakes_with_v0_9_daemon_and_omits_new_capabilities() {
    let tmp = tempdir().expect("temp dir");
    let socket = tmp.path().join("v0.9.sock");
    let listener = UnixListener::bind(&socket).expect("bind frozen daemon fixture");
    let fixture = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept client");
        let request = read_json_frame(&mut stream).await;
        assert_eq!(request["payload"]["type"], "ClientHello");
        assert_eq!(request["payload"]["client_version"], "1.0.0");
        let capabilities = request["payload"]["capabilities"]
            .as_object()
            .expect("capabilities object");
        for additive in [
            "session_library",
            "editor_actions",
            "inbox",
            "analytics",
            "automation",
            "bundles",
        ] {
            assert!(
                !capabilities.contains_key(additive),
                "v0.9 must not receive {additive}"
            );
        }
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version":{"major":1,"minor":6},
                "message_id":"22000000-0000-0000-0000-000000000001",
                "correlation_id":request["message_id"],
                "client_id":request["client_id"],
                "payload":{"type":"ServerHello","selected_protocol":{"major":1,"minor":6},
                    "daemon_version":"0.9.0","daemon_instance":"23000000-0000-0000-0000-000000000001",
                    "heartbeat_interval_ms":5000,"resume_token":"v0.9-fixture","build_id":"0.9.0-fixture"}
            }),
        )
        .await;
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version":{"major":1,"minor":6},
                "message_id":"22000000-0000-0000-0000-000000000002",
                "client_id":request["client_id"],
                "payload":{"type":"Ping"}
            }),
        )
        .await;
        let pong = read_json_frame(&mut stream).await;
        assert_eq!(pong["payload"]["type"], "Pong");
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version":{"major":1,"minor":6},
                "message_id":"22000000-0000-0000-0000-000000000003",
                "client_id":request["client_id"],
                "payload":{"type":"DaemonStatusResponse","daemon_version":"0.9.0",
                    "protocol_version":{"major":1,"minor":6},
                    "instance_id":"23000000-0000-0000-0000-000000000001","pid":1,
                    "started_at":"2026-01-01T00:00:00Z","uptime_seconds":1,"boot_count":1,
                    "database_path":"fixture.db","socket_path":"fixture.sock","session_count":0,
                    "build_id":"0.9.0-fixture","active_run_count":0}
            }),
        )
        .await;
    });

    let mut connection = Connection::connect(&socket)
        .await
        .expect("connect current client");
    let hello = tokio::time::timeout(
        Duration::from_secs(5),
        connection.handshake("current-client", "1.0.0", None),
    )
    .await
    .expect("handshake must complete")
    .expect("v0.9 daemon accepts current client");
    assert_eq!(hello.daemon_version, "0.9.0");
    assert_eq!(hello.selected_protocol.major, 1);
    assert_eq!(hello.selected_protocol.minor, 6);
    let status = tokio::time::timeout(Duration::from_secs(5), connection.next_envelope())
        .await
        .expect("heartbeat exchange must complete")
        .expect("read old-daemon status")
        .expect("old daemon remains connected");
    assert!(matches!(status.payload, Payload::DaemonStatusResponse(_)));
    tokio::time::timeout(Duration::from_secs(5), fixture)
        .await
        .expect("fixture must finish")
        .expect("fixture task");
}
