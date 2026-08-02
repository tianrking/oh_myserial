use std::sync::{Arc, Mutex};

use ohmyserial::ledger::{
    ControlPayload, EventEnvelope, EventPayload, GapCertainty, GapPayload, GapScope, Ledger,
    LedgerStatus, MemoryOptions,
};
use ohmyserial::workflow::{
    ByteValue, EvidenceCursor, LeaseReceipt, SendReceipt, WorkflowAuthorization,
    WorkflowDefinition, WorkflowError, WorkflowFuture, WorkflowPortState, WorkflowRunner,
    WorkflowRuntime, WorkflowStep,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct MockRuntime {
    ledger: Ledger,
    state: Arc<Mutex<WorkflowPortState>>,
    writes: Arc<Mutex<usize>>,
}

impl MockRuntime {
    fn new() -> Self {
        Self {
            ledger: Ledger::memory(MemoryOptions {
                max_events: 256,
                max_bytes: 1024 * 1024,
            })
            .unwrap(),
            state: Arc::new(Mutex::new(WorkflowPortState {
                connected: true,
                connection_epoch: 1,
            })),
            writes: Arc::new(Mutex::new(0)),
        }
    }

    fn append(&self, payload: EventPayload) -> EventEnvelope {
        self.ledger.append(1, payload).unwrap()
    }
}

impl WorkflowRuntime for MockRuntime {
    fn ledger_status(&self) -> LedgerStatus {
        self.ledger.status()
    }

    fn subscribe_with_cursor(
        &self,
    ) -> Result<(broadcast::Receiver<EventEnvelope>, EvidenceCursor), WorkflowError> {
        let receiver = self.ledger.subscribe();
        let status = self.ledger.status();
        Ok((
            receiver,
            EvidenceCursor {
                session_id: status.session_id,
                port_id: "default".into(),
                connection_epoch: 1,
                seq: status.newest_seq,
                byte_offset: 0,
            },
        ))
    }

    fn port_state(&self) -> WorkflowPortState {
        self.state.lock().unwrap().clone()
    }

    fn lease<'a>(
        &'a self,
        _actor: &'a str,
        _token: Option<&'a str>,
    ) -> WorkflowFuture<'a, LeaseReceipt> {
        Box::pin(async {
            Ok(LeaseReceipt {
                expires_ms: 10_000,
                lease_token: Some("opaque-test-token".into()),
            })
        })
    }

    fn send<'a>(
        &'a self,
        _actor: &'a str,
        _token: Option<&'a str>,
        bytes: Vec<u8>,
    ) -> WorkflowFuture<'a, SendReceipt> {
        let ledger = self.ledger.clone();
        let writes = self.writes.clone();
        Box::pin(async move {
            *writes.lock().unwrap() += 1;
            let event = ledger
                .append(1, EventPayload::tx("runtime", bytes.clone()))
                .map_err(|error| WorkflowError::Runtime(error.to_string()))?;
            Ok(SendReceipt {
                bytes: bytes.len(),
                tx_seq: Some(event.seq),
            })
        })
    }

    fn control<'a>(
        &'a self,
        _actor: &'a str,
        _token: Option<&'a str>,
        name: &'a str,
        _value: Option<&'a str>,
    ) -> WorkflowFuture<'a, ()> {
        let ledger = self.ledger.clone();
        let name = name.to_owned();
        Box::pin(async move {
            ledger
                .append(
                    1,
                    EventPayload::Control(ControlPayload {
                        actor: Some("runtime".into()),
                        name,
                        value: None,
                    }),
                )
                .map_err(|error| WorkflowError::Runtime(error.to_string()))?;
            Ok(())
        })
    }
}

fn auth() -> WorkflowAuthorization {
    WorkflowAuthorization {
        can_read: true,
        can_write: true,
        can_control: true,
        lease_token: None,
    }
}

#[tokio::test]
async fn runner_matches_rx_across_chunks_and_generates_server_actor() {
    let runtime = MockRuntime::new();
    let runner = WorkflowRunner::new(Default::default()).unwrap();
    let definition = WorkflowDefinition {
        id: "probe".into(),
        name: None,
        steps: vec![
            WorkflowStep::Lease,
            WorkflowStep::Send {
                bytes: ByteValue::Hex { hex: "41".into() },
            },
            WorkflowStep::Expect {
                pattern: ByteValue::Text { text: "OK".into() },
                timeout_ms: Some(500),
                capture: Some("reply".into()),
            },
        ],
    };
    let task = {
        let runtime = runtime.clone();
        let runner = runner.clone();
        let definition = definition.clone();
        tokio::spawn(async move {
            runner
                .run(
                    &runtime,
                    "request-1",
                    &definition,
                    auth(),
                    CancellationToken::new(),
                )
                .await
        })
    };
    tokio::task::yield_now().await;
    runtime.append(EventPayload::rx(b"O"));
    runtime.append(EventPayload::rx(b"K"));
    let result = task.await.unwrap().unwrap();
    assert_eq!(result.status, "succeeded");
    assert!(result.actor.starts_with("workflow:"));
    assert_eq!(result.evidence.len(), 3);
    assert_eq!(*runtime.writes.lock().unwrap(), 1);
}

#[tokio::test]
async fn client_delivery_gap_is_ignored_but_rx_gap_fails() {
    let runtime = MockRuntime::new();
    let runner = WorkflowRunner::new(Default::default()).unwrap();
    let definition = WorkflowDefinition {
        id: "gap".into(),
        name: None,
        steps: vec![WorkflowStep::Expect {
            pattern: ByteValue::Text { text: "OK".into() },
            timeout_ms: Some(500),
            capture: None,
        }],
    };
    let task = {
        let runtime = runtime.clone();
        let runner = runner.clone();
        let definition = definition.clone();
        tokio::spawn(async move {
            runner
                .run(
                    &runtime,
                    "request-gap",
                    &definition,
                    auth(),
                    CancellationToken::new(),
                )
                .await
        })
    };
    tokio::task::yield_now().await;
    runtime.append(EventPayload::Gap(GapPayload {
        scope: GapScope::ClientDelivery,
        certainty: GapCertainty::NotDelivered,
        reason: "slow client".into(),
        bytes: None,
        actor: None,
        client_ids: vec!["client".into()],
    }));
    runtime.append(EventPayload::rx(b"OK"));
    assert!(task.await.unwrap().is_ok());

    let runtime = MockRuntime::new();
    let task = {
        let runtime = runtime.clone();
        let runner = runner.clone();
        let definition = definition.clone();
        tokio::spawn(async move {
            runner
                .run(
                    &runtime,
                    "request-rx-gap",
                    &definition,
                    auth(),
                    CancellationToken::new(),
                )
                .await
        })
    };
    tokio::task::yield_now().await;
    runtime.append(EventPayload::Gap(GapPayload {
        scope: GapScope::RxObservation,
        certainty: GapCertainty::Unknown,
        reason: "driver read error".into(),
        bytes: None,
        actor: None,
        client_ids: vec![],
    }));
    assert!(matches!(task.await.unwrap(), Err(WorkflowError::RxGap(_))));
}

#[tokio::test]
async fn capability_cancellation_and_request_id_are_fail_closed() {
    let runtime = MockRuntime::new();
    let runner = WorkflowRunner::new(Default::default()).unwrap();
    let send = WorkflowDefinition {
        id: "send".into(),
        name: None,
        steps: vec![WorkflowStep::Send {
            bytes: ByteValue::Text { text: "x".into() },
        }],
    };
    let mut read_only = auth();
    read_only.can_write = false;
    assert!(matches!(
        runner
            .run(
                &runtime,
                "denied",
                &send,
                read_only,
                CancellationToken::new()
            )
            .await,
        Err(WorkflowError::WriteDenied)
    ));

    let waiting = WorkflowDefinition {
        id: "wait".into(),
        name: None,
        steps: vec![WorkflowStep::Expect {
            pattern: ByteValue::Text {
                text: "never".into(),
            },
            timeout_ms: Some(10_000),
            capture: None,
        }],
    };
    let cancellation = CancellationToken::new();
    let task = {
        let runtime = runtime.clone();
        let runner = runner.clone();
        let waiting = waiting.clone();
        let cancellation = cancellation.clone();
        tokio::spawn(async move {
            runner
                .run(&runtime, "cancelled", &waiting, auth(), cancellation)
                .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    cancellation.cancel();
    assert!(matches!(task.await.unwrap(), Err(WorkflowError::Cancelled)));

    let success = WorkflowDefinition {
        id: "success".into(),
        name: None,
        steps: vec![WorkflowStep::Assert {
            assertion: ohmyserial::workflow::WorkflowAssertion::PortConnected,
        }],
    };
    let first = runner
        .run(
            &runtime,
            "same-request",
            &success,
            auth(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let second = runner
        .run(
            &runtime,
            "same-request",
            &success,
            auth(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(first, second);
}
