use chrono::Utc;
use netband::console::{ConsoleOff, ConsoleSink};
use netband::journal::{JournalError, JournalSink, OutputCoordinator};
use netband::model::{EventKind, MeasurementEvent, Outcome, RunId, RunKind};
use std::collections::HashSet;

#[derive(Default)]
struct Records(std::cell::RefCell<Vec<MeasurementEvent>>);
impl JournalSink for Records {
    fn append_batch(&mut self, events: &[MeasurementEvent]) -> Result<(), JournalError> {
        self.0.borrow_mut().extend_from_slice(events);
        Ok(())
    }
}
impl ConsoleSink for Records {
    fn offer(&self, event: &MeasurementEvent) {
        self.0.borrow_mut().push(event.clone());
    }
}

#[test]
fn hierarchy_and_emission_order_survive_tied_and_reversed_clocks() {
    let mut output = OutputCoordinator::new(Records::default(), Records::default());
    let session = RunId::new();
    let ping = RunId::new();
    let bandwidth = RunId::new();
    output.start_session(session, "run").unwrap();
    output.start_run(ping, session, RunKind::PingRound).unwrap();
    output
        .start_run(bandwidth, session, RunKind::Bandwidth)
        .unwrap();
    let tied = Utc::now() - chrono::Duration::days(1);
    for (run, kind) in [
        (bandwidth, EventKind::Bandwidth),
        (ping, EventKind::PingProbe),
    ] {
        let event = MeasurementEvent::new(run, kind, Outcome::Success, tied);
        output.publish_batch(&[event]).unwrap();
        output.finish_run(run, Outcome::Success, None).unwrap();
    }
    output
        .finish_run(session, Outcome::Cancelled, None)
        .unwrap();
    let (journal, console) = output.into_parts();
    let journal = journal.0.into_inner();
    let console = console.0.into_inner();
    assert_eq!(journal, console);
    let mut started = HashSet::new();
    let mut ids = HashSet::new();
    for (index, event) in journal.iter().enumerate() {
        assert_eq!(event.event_sequence, Some(index as u64 + 1));
        assert!(ids.insert(event.event_id));
        if let Some(parent) = event.parent_run_id {
            assert!(started.contains(&parent));
            assert_eq!(parent, session);
        }
        if event.event_kind == EventKind::RunStarted {
            assert!(started.insert(event.run_id));
        }
        assert!(started.contains(&event.run_id));
    }
    let root = &journal[0];
    assert_eq!(root.command.as_deref(), Some("run"));
    assert_eq!(
        root.netband_version.as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(root.process_id, Some(std::process::id()));
    assert!(root.parent_run_id.is_none());
    assert!(root.finished_at_utc.is_none());
    assert!(journal.last().unwrap().elapsed_ms.unwrap() >= 0.0);
}

#[test]
fn uuid_roles_are_canonical_and_distinct() {
    use netband::model::{EventId, RequestId};
    let run = RunId::new();
    let event = EventId::new();
    let request = RequestId::new();
    let values = [run.to_string(), event.to_string(), request.to_string()];
    for value in &values {
        let parsed = uuid::Uuid::parse_str(value).unwrap();
        assert_eq!(parsed.get_version_num(), 4);
        assert_eq!(parsed.to_string(), *value);
    }
    assert_eq!(values.iter().collect::<HashSet<_>>().len(), 3);
    assert_eq!(serde_json::to_string(&run).unwrap(), format!("\"{run}\""));
}

#[test]
fn failed_start_is_not_offered_to_console() {
    struct Broken;
    impl JournalSink for Broken {
        fn append_batch(&mut self, _: &[MeasurementEvent]) -> Result<(), JournalError> {
            Err(JournalError::write(std::io::Error::other("fixture")))
        }
    }
    let mut output = OutputCoordinator::new(Broken, Records::default());
    assert!(output.start_session(RunId::new(), "once ping").is_err());
    assert!(output.into_parts().1.0.borrow().is_empty());
}

#[test]
fn unfinished_run_has_no_synthetic_finish() {
    let mut output = OutputCoordinator::new(Records::default(), ConsoleOff);
    output.start_session(RunId::new(), "run").unwrap();
    let (journal, _) = output.into_parts();
    let journal = journal.0.into_inner();
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0].event_kind, EventKind::RunStarted);
}

#[test]
fn parent_cannot_finish_before_its_children_and_new_sessions_reset_order() {
    let mut output = OutputCoordinator::new(Records::default(), ConsoleOff);
    let session = RunId::new();
    let child = RunId::new();
    assert!(
        output
            .start_run(child, session, RunKind::PingRound)
            .is_err()
    );
    output.start_session(session, "run").unwrap();
    output
        .start_run(child, session, RunKind::PingRound)
        .unwrap();
    assert!(output.finish_run(session, Outcome::Success, None).is_err());
    output.finish_run(child, Outcome::Success, None).unwrap();
    output
        .finish_run(session, Outcome::Cancelled, None)
        .unwrap();
    output.start_session(RunId::new(), "once ping").unwrap();
    assert_eq!(
        output
            .into_parts()
            .0
            .0
            .into_inner()
            .last()
            .unwrap()
            .event_sequence,
        Some(1)
    );
}

#[test]
fn lifecycle_csv_and_json_have_the_same_fields_and_values() {
    use netband::journal::{CSV_HEADER, JournalWriter};
    let journal = JournalWriter::from_writer(Vec::new()).unwrap();
    let mut output = OutputCoordinator::new(journal, Records::default());
    let session = RunId::new();
    let child = RunId::new();
    output.start_session(session, "once bandwidth").unwrap();
    output
        .start_run(child, session, RunKind::Bandwidth)
        .unwrap();
    output
        .finish_run(child, Outcome::Error, Some("fixture key=secret"))
        .unwrap();
    output
        .finish_run(session, Outcome::Error, Some("fixture key=secret"))
        .unwrap();
    let (journal, console) = output.into_parts();
    let bytes = journal.into_inner().unwrap();
    let json = console.0.into_inner();
    let mut reader = csv::Reader::from_reader(bytes.as_slice());
    for (row, event) in reader.records().zip(json) {
        let row = row.unwrap();
        let value = serde_json::to_value(event.sanitized()).unwrap();
        assert_eq!(value.as_object().unwrap().len(), row.len());
        for (field, cell) in CSV_HEADER.split(',').zip(row.iter()) {
            match &value[field] {
                serde_json::Value::Null => assert!(cell.is_empty(), "{field}"),
                serde_json::Value::String(text) => assert_eq!(cell, text, "{field}"),
                number => assert_eq!(
                    cell.parse::<f64>().unwrap(),
                    number.as_f64().unwrap(),
                    "{field}"
                ),
            }
        }
    }
    assert!(!String::from_utf8(bytes).unwrap().contains("secret"));
}

#[test]
fn failed_publication_does_not_attempt_more_writes_or_publish_a_finish() {
    use std::cell::Cell;
    use std::rc::Rc;
    struct FailsAfterStart(Rc<Cell<usize>>);
    impl JournalSink for FailsAfterStart {
        fn append_batch(&mut self, _: &[MeasurementEvent]) -> Result<(), JournalError> {
            self.0.set(self.0.get() + 1);
            if self.0.get() == 1 {
                Ok(())
            } else {
                Err(JournalError::write(std::io::Error::other("disk full")))
            }
        }
    }
    let calls = Rc::new(Cell::new(0));
    let mut output = OutputCoordinator::new(FailsAfterStart(calls.clone()), Records::default());
    let session = RunId::new();
    output.start_session(session, "run").unwrap();
    assert!(
        output
            .start_run(RunId::new(), session, RunKind::PingRound)
            .is_err()
    );
    assert!(
        output
            .finish_session(session, Outcome::Error, Some("disk full"))
            .is_err()
    );
    assert_eq!(calls.get(), 2);
    let events = output.into_parts().1.0.into_inner();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_kind, EventKind::RunStarted);
}

#[test]
fn handled_error_finishes_active_children_before_the_session() {
    let mut output = OutputCoordinator::new(Records::default(), ConsoleOff);
    let session = RunId::new();
    let child = RunId::new();
    output.start_session(session, "run").unwrap();
    output
        .start_run(child, session, RunKind::Bandwidth)
        .unwrap();
    output
        .finish_session(
            session,
            Outcome::Error,
            Some("scheduler persistence failed"),
        )
        .unwrap();
    let events = output.into_parts().0.0.into_inner();
    assert_eq!(events[2].run_id, child);
    assert_eq!(events[3].run_id, session);
    for event in &events[2..] {
        assert_eq!(event.event_kind, EventKind::RunFinished);
        assert_eq!(event.outcome, Outcome::Error);
        assert!(
            event
                .message
                .as_deref()
                .unwrap()
                .contains("persistence failed")
        );
    }
}
