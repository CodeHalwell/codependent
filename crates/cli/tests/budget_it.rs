//! `codypendent budget …` — the client half of `ManageAnalyticsBudget`.
//!
//! Like `session_library_it.rs`, this drives the connected core in
//! `codypendent_cli::commands` against a hand-rolled mock daemon built only from
//! `codypendent_protocol`'s framing — no `codypendentd` subprocess.
//!
//! The load-bearing assertion is the one that would have caught the defect this
//! surface exists to close: the daemon refuses every budget MUTATION without the
//! `Controller` role (`protocol.role-denied`), so a client that does not bind it
//! can never create a budget, and with no budget the threshold evaluator in
//! `ledger::append_run_terminal` has nothing to evaluate. Every test below
//! asserts the bind crossed the wire before the command did, and the refusal
//! paths assert the CLI FAILS rather than reporting a budget it did not get.

use std::time::Duration;

use codypendent_cli::connection::Connection;
use codypendent_protocol::{
    read_envelope, write_envelope, AnalyticsBudget, AnalyticsBudgetDimension, AnalyticsBudgetDraft,
    AnalyticsBudgetPage, AnalyticsBudgetPatch, AnalyticsBudgetQuery, AnalyticsBudgetRequest,
    AnalyticsBudgetScope, AnalyticsBudgetWindow, ClientRole, CodypendentError, Command,
    CommandBody, CommandId, DaemonInstanceId, Envelope, Payload, ServerHello, PROTOCOL_V1,
};
use tokio::net::{UnixListener, UnixStream};

struct MockSocket {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
}

impl MockSocket {
    fn bind() -> (Self, UnixListener) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("d.sock");
        let listener = UnixListener::bind(&path).expect("bind mock socket");
        (Self { _dir: dir, path }, listener)
    }
}

fn expect_command(request: &Envelope) -> &Command {
    match &request.payload {
        Payload::Command(command) => command,
        other => panic!("expected a Command envelope, got {other:?}"),
    }
}

fn command_id_of(request: &Envelope) -> CommandId {
    expect_command(request).command_id
}

async fn accept(listener: &UnixListener) -> UnixStream {
    let (stream, _addr) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("mock accepted a connection in time")
        .expect("accept");
    stream
}

/// Handshake, then the `AttachSession` that binds the role. Asserts the client
/// asked for `Controller` — without it every budget mutation is role-denied and
/// the whole feature stays unreachable.
async fn accept_handshake_and_role_bind(stream: &mut UnixStream) {
    let hello = read_envelope(stream)
        .await
        .expect("read ClientHello")
        .expect("connection open");
    assert!(matches!(hello.payload, Payload::ClientHello(_)));
    write_envelope(
        stream,
        &Envelope::reply_to(
            &hello,
            Payload::ServerHello(ServerHello {
                resume_token: None,
                selected_protocol: PROTOCOL_V1,
                daemon_version: "mock".to_string(),
                daemon_instance: DaemonInstanceId::new(),
                heartbeat_interval_ms: 15_000,
                build_id: String::new(),
            }),
        ),
    )
    .await
    .expect("write ServerHello");

    let attach = read_envelope(stream)
        .await
        .expect("read AttachSession")
        .expect("connection open");
    match &expect_command(&attach).body {
        CommandBody::AttachSession { requested_role, .. } => {
            assert_eq!(
                *requested_role,
                ClientRole::Controller,
                "creating, updating and deleting a budget all sit behind the daemon's \
                 Controller check; a client that does not bind it is refused invisibly"
            );
        }
        other => panic!("expected AttachSession, got {other:?}"),
    }
    write_envelope(
        stream,
        &Envelope::reply_to(
            &attach,
            Payload::CommandRejected(CodypendentError::new(
                "protocol.session-not-found",
                "unknown session",
                false,
            )),
        ),
    )
    .await
    .expect("write attach rejection");
}

fn stored(id: &str, definition: AnalyticsBudgetDraft) -> AnalyticsBudget {
    AnalyticsBudget {
        id: id.to_string(),
        definition,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn draft(scope: AnalyticsBudgetScope, threshold: u64, enabled: bool) -> AnalyticsBudgetDraft {
    AnalyticsBudgetDraft {
        scope,
        dimension: AnalyticsBudgetDimension::CostMicros,
        window: AnalyticsBudgetWindow::Day,
        threshold,
        enabled,
    }
}

#[tokio::test]
async fn budget_create_binds_controller_and_reports_the_daemons_stored_budget() {
    let (socket, listener) = MockSocket::bind();
    let sent = draft(
        AnalyticsBudgetScope::Repository {
            repository_id: "/repo".to_string(),
        },
        5_000_000,
        true,
    );
    let sent_for_server = sent.clone();

    let server = tokio::spawn(async move {
        let mut stream = accept(&listener).await;
        accept_handshake_and_role_bind(&mut stream).await;

        let request = read_envelope(&mut stream)
            .await
            .expect("read ManageAnalyticsBudget")
            .expect("connection open");
        let command_id = command_id_of(&request);
        match &expect_command(&request).body {
            CommandBody::ManageAnalyticsBudget {
                request: AnalyticsBudgetRequest::Create { budget },
            } => {
                assert_eq!(*budget, sent_for_server);
                // The owner is NEVER on the wire: it is the connection's
                // kernel-derived principal.
                assert!(
                    !serde_json::to_string(budget)
                        .expect("serialize")
                        .contains("owner"),
                    "a client must not be able to name whose budget this is"
                );
            }
            other => panic!("expected a budget Create, got {other:?}"),
        }
        // The daemon minted the id and stored a DIFFERENT enabled flag than the
        // client asked for (an operator policy could). The CLI must report this.
        write_envelope(
            &mut stream,
            &Envelope::reply_to(
                &request,
                Payload::AnalyticsBudgetResult {
                    command_id,
                    budget: stored(
                        "bdg-7",
                        draft(
                            AnalyticsBudgetScope::Repository {
                                repository_id: "/repo".to_string(),
                            },
                            5_000_000,
                            false,
                        ),
                    ),
                },
            ),
        )
        .await
        .expect("write AnalyticsBudgetResult");
    });

    let mut conn = Connection::connect(&socket.path).await.expect("connect");
    let mut out: Vec<u8> = Vec::new();
    codypendent_cli::commands::budget_manage_over_connection(
        &mut conn,
        AnalyticsBudgetRequest::Create { budget: sent },
        false,
        &mut out,
    )
    .await
    .expect("budget create");
    server.await.expect("mock server task");

    let printed = String::from_utf8(out).expect("utf8");
    assert!(printed.contains("bdg-7"), "{printed}");
    assert!(printed.contains("repository=/repo"), "{printed}");
    assert!(printed.contains("cost_micros"), "{printed}");
    assert!(
        printed.contains("disabled"),
        "the daemon's projection is reported, not the draft that was sent: {printed}"
    );
}

#[tokio::test]
async fn a_refused_budget_create_fails_instead_of_printing_a_budget_that_was_never_stored() {
    let (socket, listener) = MockSocket::bind();
    let server = tokio::spawn(async move {
        let mut stream = accept(&listener).await;
        accept_handshake_and_role_bind(&mut stream).await;
        let request = read_envelope(&mut stream)
            .await
            .expect("read ManageAnalyticsBudget")
            .expect("connection open");
        write_envelope(
            &mut stream,
            &Envelope::reply_to(
                &request,
                Payload::CommandRejected(CodypendentError::new(
                    "protocol.role-denied",
                    "creating an analytics budget requires the Controller role",
                    false,
                )),
            ),
        )
        .await
        .expect("write rejection");
    });

    let mut conn = Connection::connect(&socket.path).await.expect("connect");
    let mut out: Vec<u8> = Vec::new();
    let error = codypendent_cli::commands::budget_manage_over_connection(
        &mut conn,
        AnalyticsBudgetRequest::Create {
            budget: draft(AnalyticsBudgetScope::Owner, 1_000, true),
        },
        false,
        &mut out,
    )
    .await
    .expect_err("a refused create must fail");
    server.await.expect("mock server task");

    assert!(
        error.to_string().contains("protocol.role-denied"),
        "the daemon's own code reaches the operator: {error}"
    );
    assert!(
        out.is_empty(),
        "nothing is printed for a budget that was never stored"
    );
}

#[tokio::test]
async fn budget_list_reports_the_daemons_truncation_and_never_flattens_an_unknown_scope() {
    let (socket, listener) = MockSocket::bind();
    let server = tokio::spawn(async move {
        let mut stream = accept(&listener).await;
        accept_handshake_and_role_bind(&mut stream).await;
        let request = read_envelope(&mut stream)
            .await
            .expect("read ManageAnalyticsBudget")
            .expect("connection open");
        let command_id = command_id_of(&request);
        match &expect_command(&request).body {
            CommandBody::ManageAnalyticsBudget {
                request: AnalyticsBudgetRequest::List { query },
            } => {
                assert_eq!(query.enabled, Some(true));
                assert_eq!(query.limit, 3);
            }
            other => panic!("expected a budget List, got {other:?}"),
        }
        write_envelope(
            &mut stream,
            &Envelope::reply_to(
                &request,
                Payload::AnalyticsBudgetPage {
                    command_id,
                    page: AnalyticsBudgetPage {
                        items: vec![
                            stored("bdg-1", draft(AnalyticsBudgetScope::Owner, 10, true)),
                            // A scope a NEWER daemon knows and this build does
                            // not.
                            stored("bdg-2", draft(AnalyticsBudgetScope::Unknown, 20, true)),
                        ],
                        truncated: true,
                    },
                },
            ),
        )
        .await
        .expect("write AnalyticsBudgetPage");
    });

    let mut conn = Connection::connect(&socket.path).await.expect("connect");
    let mut out: Vec<u8> = Vec::new();
    codypendent_cli::commands::budget_manage_over_connection(
        &mut conn,
        AnalyticsBudgetRequest::List {
            query: AnalyticsBudgetQuery {
                enabled: Some(true),
                limit: 3,
            },
        },
        false,
        &mut out,
    )
    .await
    .expect("budget list");
    server.await.expect("mock server task");

    let printed = String::from_utf8(out).expect("utf8");
    assert!(
        printed.contains("bdg-1") && printed.contains("bdg-2"),
        "{printed}"
    );
    assert!(
        printed.contains("unknown-scope"),
        "an unrecognized scope is named as unknown: {printed}"
    );
    assert_eq!(
        printed.matches("owner").count(),
        1,
        "the unknown scope is NOT flattened onto owner, which would claim it \
         covers everything the operator runs: {printed}"
    );
    assert!(
        printed.contains("cut this listing short"),
        "a page the daemon truncated must not read as the whole set: {printed}"
    );
}

#[tokio::test]
async fn budget_delete_prints_the_daemons_receipt() {
    let (socket, listener) = MockSocket::bind();
    let server = tokio::spawn(async move {
        let mut stream = accept(&listener).await;
        accept_handshake_and_role_bind(&mut stream).await;
        let request = read_envelope(&mut stream)
            .await
            .expect("read ManageAnalyticsBudget")
            .expect("connection open");
        let command_id = command_id_of(&request);
        match &expect_command(&request).body {
            CommandBody::ManageAnalyticsBudget {
                request: AnalyticsBudgetRequest::Delete { id },
            } => assert_eq!(id, "bdg-9"),
            other => panic!("expected a budget Delete, got {other:?}"),
        }
        write_envelope(
            &mut stream,
            &Envelope::reply_to(
                &request,
                Payload::AnalyticsBudgetDeleted {
                    command_id,
                    budget_id: "bdg-9".to_string(),
                },
            ),
        )
        .await
        .expect("write AnalyticsBudgetDeleted");
    });

    let mut conn = Connection::connect(&socket.path).await.expect("connect");
    let mut out: Vec<u8> = Vec::new();
    codypendent_cli::commands::budget_manage_over_connection(
        &mut conn,
        AnalyticsBudgetRequest::Delete {
            id: "bdg-9".to_string(),
        },
        false,
        &mut out,
    )
    .await
    .expect("budget delete");
    server.await.expect("mock server task");

    assert!(
        String::from_utf8(out)
            .expect("utf8")
            .contains("bdg-9 deleted"),
        "the deletion receipt is the daemon's, not an assumption"
    );
}

#[tokio::test]
async fn an_unauthorized_budget_is_indistinguishable_from_an_absent_one() {
    // Both a budget that does not exist and one owned by someone else are
    // answered by the daemon's ownership gate with the SAME generic error. The
    // CLI must relay it verbatim and add nothing that would tell the two apart.
    let mut messages = Vec::new();
    for id in ["definitely-absent", "someone-elses"] {
        let (socket, listener) = MockSocket::bind();
        let server = tokio::spawn(async move {
            let mut stream = accept(&listener).await;
            accept_handshake_and_role_bind(&mut stream).await;
            let request = read_envelope(&mut stream)
                .await
                .expect("read ManageAnalyticsBudget")
                .expect("connection open");
            write_envelope(
                &mut stream,
                &Envelope::reply_to(
                    &request,
                    Payload::CommandRejected(CodypendentError::new(
                        "analytics.budget-not-found",
                        "analytics budget is unavailable",
                        false,
                    )),
                ),
            )
            .await
            .expect("write rejection");
        });

        let mut conn = Connection::connect(&socket.path).await.expect("connect");
        let mut out: Vec<u8> = Vec::new();
        let error = codypendent_cli::commands::budget_manage_over_connection(
            &mut conn,
            AnalyticsBudgetRequest::Get { id: id.to_string() },
            false,
            &mut out,
        )
        .await
        .expect_err("a refused get must fail");
        server.await.expect("mock server task");
        assert!(
            out.is_empty(),
            "nothing is printed for a budget not returned"
        );
        messages.push(error.to_string());
    }
    assert_eq!(
        messages[0], messages[1],
        "the CLI must not turn the daemon's single generic refusal into an \
         existence oracle: {messages:?}"
    );
}

#[tokio::test]
async fn an_empty_update_is_refused_before_anything_reaches_the_daemon() {
    let (socket, listener) = MockSocket::bind();
    // The mock accepts nothing: a patch with no fields must never open a
    // conversation, because the daemon would answer with the UNCHANGED budget
    // and printing that would report a change that never happened.
    let mut conn = Connection::connect(&socket.path).await.expect("connect");
    let mut out: Vec<u8> = Vec::new();
    let error = codypendent_cli::commands::budget_manage_over_connection(
        &mut conn,
        AnalyticsBudgetRequest::Update {
            id: "bdg-1".to_string(),
            patch: AnalyticsBudgetPatch::default(),
        },
        false,
        &mut out,
    )
    .await
    .expect_err("an empty patch must fail");
    assert!(error.to_string().contains("nothing to update"), "{error}");
    assert!(out.is_empty());
    drop(listener);
}

#[tokio::test]
async fn an_unknown_budget_request_is_refused_rather_than_sent() {
    // `AnalyticsBudgetRequest` is `#[non_exhaustive]` with a `#[serde(other)]`
    // `Unknown`. A client must not put a body on the wire whose meaning it does
    // not know — it fails closed instead.
    let (socket, listener) = MockSocket::bind();
    let mut conn = Connection::connect(&socket.path).await.expect("connect");
    let mut out: Vec<u8> = Vec::new();
    let error = codypendent_cli::commands::budget_manage_over_connection(
        &mut conn,
        AnalyticsBudgetRequest::Unknown,
        false,
        &mut out,
    )
    .await
    .expect_err("an unknown request must fail");
    assert!(error.to_string().contains("does not recognize"), "{error}");
    assert!(out.is_empty());
    drop(listener);
}
