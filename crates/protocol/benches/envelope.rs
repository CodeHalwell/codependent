//! Envelope encode/decode — the cost paid on EVERY message in and out of the
//! daemon.
//!
//! `framing::write_envelope` is `serde_json::to_vec` plus a 4-byte big-endian
//! length prefix and two `write_all`s; `read_envelope` is `read_exact` plus
//! `serde_json::from_slice`. The framing arithmetic is a handful of
//! instructions and the socket writes are not this crate's to optimise, so what
//! is measured here is the serde half — the part that actually scales with the
//! message, and the part a payload-shape change would regress.
//!
//! Determinism: every identifier is built from a FIXED `u128` rather than
//! `Uuid::now_v7()`, and the timestamp is a fixed instant rather than
//! `Utc::now()`. Two runs of this bench therefore serialize byte-identical
//! input. Nothing here touches the network, the clock, the filesystem, or any
//! global.

use chrono::{DateTime, TimeZone, Utc};
use codypendent_protocol::envelope::{Envelope, Payload};
use codypendent_protocol::events::{Actor, EventBody, SessionEvent};
use codypendent_protocol::ids::{AgentId, ClientId, MessageId, ModelId, RunId};
use codypendent_protocol::version::PROTOCOL_V1;
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use std::hint::black_box;

fn fixed_time() -> DateTime<Utc> {
    Utc.timestamp_opt(1_765_000_000, 0).single().expect("fixed")
}

/// One streamed token's worth of ASCII prose — the shape of the overwhelming
/// majority of frames during a run.
const ASCII_DELTA: &str = "the quick brown fox jumps over the lazy dog";

/// The same budget of text in a script where one grapheme is three UTF-8 bytes,
/// plus a ZWJ emoji sequence. JSON string escaping and UTF-8 validation on the
/// decode side both behave differently here, so a bench that only ever saw
/// ASCII would miss a regression that only multi-byte text triggers.
const WIDE_DELTA: &str = "配置ファイルを読み込んでいます。テストは全て通過しました 👨‍👩‍👧‍👦 🇯🇵 完了";

fn envelope_with(body: EventBody) -> Envelope {
    Envelope {
        protocol_version: PROTOCOL_V1,
        message_id: MessageId(uuid::Uuid::from_u128(
            0x1111_2222_3333_4444_5555_6666_7777_8888,
        )),
        correlation_id: None,
        client_id: ClientId(uuid::Uuid::from_u128(
            0x0f0f_0f0f_0f0f_0f0f_0f0f_0f0f_0f0f_0f0f,
        )),
        workspace_id: None,
        session_id: None,
        sequence: Some(4_211),
        payload: Payload::Event(SessionEvent {
            sequence: 4_211,
            occurred_at: fixed_time(),
            causation_id: None,
            correlation_id: None,
            actor: Actor::Agent {
                agent_id: AgentId(uuid::Uuid::from_u128(
                    0xdead_beef_dead_beef_dead_beef_dead_beef,
                )),
                run_id: RunId(uuid::Uuid::from_u128(
                    0xabcd_ef01_2345_6789_abcd_ef01_2345_6789,
                )),
                model: ModelId("anthropic/claude-opus-4".to_string()),
            },
            body,
        }),
    }
}

fn delta(text: &str) -> Envelope {
    envelope_with(EventBody::ModelStreamDelta {
        run_id: RunId(uuid::Uuid::from_u128(
            0xabcd_ef01_2345_6789_abcd_ef01_2345_6789,
        )),
        text: text.to_string(),
    })
}

fn bench_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("envelope/encode");

    // The floor: a control frame with no payload data at all. Anything above
    // this in the other cases is the payload's own cost, not the envelope's.
    let ping = Envelope {
        protocol_version: PROTOCOL_V1,
        message_id: MessageId(uuid::Uuid::from_u128(1)),
        correlation_id: None,
        client_id: ClientId(uuid::Uuid::from_u128(2)),
        workspace_id: None,
        session_id: None,
        sequence: None,
        payload: Payload::Ping,
    };
    group.bench_function("ping", |b| {
        b.iter(|| serde_json::to_vec(black_box(&ping)).expect("encode"))
    });

    for (name, text) in [("ascii", ASCII_DELTA), ("wide", WIDE_DELTA)] {
        let env = delta(text);
        let encoded = serde_json::to_vec(&env).expect("encode");
        group.throughput(Throughput::Bytes(encoded.len() as u64));
        group.bench_function(format!("stream_delta/{name}"), |b| {
            b.iter(|| serde_json::to_vec(black_box(&env)).expect("encode"))
        });
    }

    group.finish();
}

fn bench_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("envelope/decode");

    for (name, text) in [("ascii", ASCII_DELTA), ("wide", WIDE_DELTA)] {
        let encoded = serde_json::to_vec(&delta(text)).expect("encode");
        group.throughput(Throughput::Bytes(encoded.len() as u64));
        group.bench_function(format!("stream_delta/{name}"), |b| {
            b.iter(|| serde_json::from_slice::<Envelope>(black_box(&encoded)).expect("decode"))
        });
    }

    group.finish();
}

/// A catch-up reply carries a whole batch of events in ONE frame, so its cost
/// is not "one message" — it is the only place the per-message numbers above
/// get multiplied by a number the operator does not control.
fn bench_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("envelope/batch");

    for count in [64usize, 1_024] {
        let events: Vec<Envelope> = (0..count).map(|_| delta(ASCII_DELTA)).collect();
        let encoded: Vec<Vec<u8>> = events
            .iter()
            .map(|e| serde_json::to_vec(e).expect("encode"))
            .collect();
        let bytes: usize = encoded.iter().map(Vec::len).sum();
        group.throughput(Throughput::Bytes(bytes as u64));

        group.bench_function(format!("encode/{count}_events"), |b| {
            b.iter(|| {
                let mut out = Vec::with_capacity(bytes);
                for e in black_box(&events) {
                    serde_json::to_writer(&mut out, e).expect("encode");
                }
                out
            })
        });

        group.bench_function(format!("decode/{count}_events"), |b| {
            b.iter_batched(
                || &encoded,
                |encoded| {
                    encoded
                        .iter()
                        .map(|b| serde_json::from_slice::<Envelope>(b).expect("decode"))
                        .count()
                },
                BatchSize::SmallInput,
            )
        });
    }

    group.finish();
}

criterion_group!(benches, bench_encode, bench_decode, bench_batch);
criterion_main!(benches);
