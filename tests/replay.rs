use std::{fs, path::Path, time::Duration};

use ohmyserial::{
    ledger::{EventEnvelope, EventPayload, Ledger, LedgerOptions, MemoryOptions, StoreOptions},
    replay::{
        ReplayError, ReplayMode, ReplayOptions, ReplaySession, MAX_REPLAY_SPEED, MIN_REPLAY_SPEED,
    },
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn captured_events() -> Vec<EventEnvelope> {
    let ledger = Ledger::memory(MemoryOptions {
        max_events: 16,
        max_bytes: 1024 * 1024,
    })
    .unwrap();
    let mut events = vec![
        ledger.append(3, EventPayload::rx(b"one")).unwrap(),
        ledger
            .append(3, EventPayload::tx("fixture", b"two"))
            .unwrap(),
        ledger.append(4, EventPayload::rx(b"three")).unwrap(),
    ];
    events[0].mono_us = 10;
    events[1].mono_us = 40_010;
    events[2].mono_us = 90_010;
    events
}

fn persisted_capture() -> (TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(LedgerOptions {
        memory: MemoryOptions {
            max_events: 16,
            max_bytes: 1024 * 1024,
        },
        stream_capacity: 16,
        store: Some(StoreOptions {
            directory: directory.path().to_path_buf(),
            segment_max_bytes: 1024 * 1024,
            segment_max_events: 100,
            flush_every_events: 1,
            fsync_on_flush: false,
        }),
        ..LedgerOptions::default()
    })
    .unwrap();
    ledger.append(1, EventPayload::rx(b"one")).unwrap();
    ledger.append(1, EventPayload::rx(b"two")).unwrap();
    ledger.seal().unwrap();
    drop(ledger);

    let segment = fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("omslog"))
        .expect("sealed segment");
    (directory, segment)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[tokio::test]
async fn immediate_replay_preserves_envelope_order() {
    let expected = captured_events();
    let session = ReplaySession::from_envelopes(expected.clone()).unwrap();
    let mut cursor = session.cursor(ReplayOptions::default()).unwrap();
    let mut actual = Vec::new();
    while let Some(event) = cursor.next_event().await.unwrap() {
        actual.push(event);
    }

    assert_eq!(actual, expected);
    assert!(cursor.is_finished());
}

#[tokio::test]
async fn callback_receives_original_envelopes() {
    let expected = captured_events();
    let session = ReplaySession::from_envelopes(expected.clone()).unwrap();
    let mut actual = Vec::new();
    let report = session
        .play(ReplayOptions::default(), |event| actual.push(event))
        .await
        .unwrap();

    assert_eq!(actual, expected);
    assert_eq!(report.emitted, expected.len());
    assert_eq!(report.first_seq, Some(expected[0].seq));
    assert_eq!(report.last_seq, Some(expected[2].seq));
}

#[tokio::test]
async fn original_replay_waits_for_recorded_interval_at_one_x() {
    let session = ReplaySession::from_envelopes(captured_events()).unwrap();
    let mut cursor = session
        .cursor(ReplayOptions {
            mode: ReplayMode::Original,
            speed: 1.0,
        })
        .unwrap();

    assert_eq!(cursor.delay_before_next().unwrap(), Some(Duration::ZERO));
    cursor.next_event().await.unwrap().unwrap();
    assert_eq!(
        cursor.delay_before_next().unwrap(),
        Some(Duration::from_millis(40))
    );

    let started = tokio::time::Instant::now();
    cursor.next_event().await.unwrap().unwrap();
    assert!(started.elapsed() >= Duration::from_millis(40));

    let mut fast_cursor = session
        .cursor(ReplayOptions {
            mode: ReplayMode::Original,
            speed: 2.0,
        })
        .unwrap();
    fast_cursor.next_event().await.unwrap().unwrap();
    assert_eq!(
        fast_cursor.delay_before_next().unwrap(),
        Some(Duration::from_millis(20))
    );
}

#[test]
fn manual_replay_steps_exactly_n_events() {
    let expected = captured_events();
    let session = ReplaySession::from_envelopes(expected.clone()).unwrap();
    let mut cursor = session.manual_cursor();

    assert_eq!(cursor.step(2).unwrap(), expected[..2]);
    assert_eq!(cursor.position(), 2);
    assert_eq!(cursor.remaining(), 1);
    assert_eq!(cursor.step(20).unwrap(), expected[2..]);
    assert!(cursor.step(1).unwrap().is_empty());
    assert!(cursor.is_finished());
    assert!(matches!(
        cursor.delay_before_next().unwrap_err(),
        ReplayError::ManualStepRequired
    ));
}

#[test]
fn discontinuous_sequence_is_rejected() {
    let mut events = captured_events();
    events[1].seq += 1;
    let error = ReplaySession::from_envelopes(events).unwrap_err();
    assert!(matches!(
        error,
        ReplayError::SequenceDiscontinuity {
            expected: 2,
            actual: 3
        }
    ));
}

#[test]
fn verified_file_and_directory_load_but_bad_hash_is_rejected() {
    let (directory, segment) = persisted_capture();
    assert_eq!(ReplaySession::load(directory.path()).unwrap().len(), 2);
    assert_eq!(ReplaySession::load(&segment).unwrap().len(), 2);

    let original = fs::read_to_string(&segment).unwrap();
    let tampered = original.replacen("\"data_base64\":\"b25l\"", "\"data_base64\":\"b25m\"", 1);
    assert_ne!(tampered, original, "fixture payload should be present");
    fs::write(&segment, tampered).unwrap();

    let error = ReplaySession::load(&segment).unwrap_err();
    let ReplayError::Read { detail, .. } = error else {
        panic!("expected a verified-reader error")
    };
    assert!(
        detail.to_ascii_lowercase().contains("hash"),
        "unexpected error: {detail}"
    );
}

#[test]
fn valid_hash_does_not_hide_a_sequence_gap() {
    let (_directory, segment) = persisted_capture();
    let text = fs::read_to_string(&segment).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let event_lines: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.contains("\"record\":\"event\"").then_some(index))
        .collect();
    assert_eq!(event_lines.len(), 2);
    let second = event_lines[1];
    lines[second] = lines[second].replacen("\"seq\":2", "\"seq\":3", 1);

    let footer_index = lines
        .iter()
        .position(|line| line.contains("\"record\":\"footer\""))
        .expect("footer line");
    let mut hashed_content = lines[..footer_index].join("\n");
    hashed_content.push('\n');
    let mut footer: serde_json::Value = serde_json::from_str(&lines[footer_index]).unwrap();
    footer["last_seq"] = serde_json::json!(3);
    footer["content_sha256"] = serde_json::json!(sha256_hex(hashed_content.as_bytes()));
    lines[footer_index] = serde_json::to_string(&footer).unwrap();
    let mut rewritten = lines.join("\n");
    rewritten.push('\n');
    fs::write(&segment, rewritten).unwrap();

    let error = ReplaySession::load(&segment).unwrap_err();
    let ReplayError::Read { detail, .. } = error else {
        panic!("expected a verified-reader error")
    };
    assert!(
        detail.to_ascii_lowercase().contains("seq"),
        "unexpected error: {detail}"
    );
}

#[test]
fn speed_is_finite_and_bounded() {
    let session = ReplaySession::from_envelopes(captured_events()).unwrap();
    for speed in [
        f64::NAN,
        f64::INFINITY,
        MIN_REPLAY_SPEED / 2.0,
        MAX_REPLAY_SPEED * 2.0,
    ] {
        let error = session
            .cursor(ReplayOptions {
                mode: ReplayMode::Original,
                speed,
            })
            .unwrap_err();
        assert!(matches!(error, ReplayError::InvalidSpeed { .. }));
    }
}

#[test]
fn replay_source_has_no_live_routing_dependencies() {
    let replay_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/replay");
    let mut source = String::new();
    for entry in fs::read_dir(replay_root).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            source.push_str(&fs::read_to_string(path).unwrap());
        }
    }

    for forbidden in ["Broker", "SerialHub", "RealPortConfig"] {
        assert!(
            !source.contains(forbidden),
            "read-only replay must not depend on {forbidden}"
        );
    }
}
