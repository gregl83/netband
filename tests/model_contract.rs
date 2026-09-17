use netband::model::sanitize_message;

#[test]
fn message_sanitization_redacts_field_keys_without_matching_word_suffixes() {
    for (input, expected) in [
        (
            "monkey=banana hockey=score turnkey=value",
            "monkey=banana hockey=score turnkey=value",
        ),
        (
            "key=one ?key=two&next=ok",
            "key=[redacted] ?key=[redacted]&next=ok",
        ),
        (
            "secret_key=one API_KEY=two",
            "secret_key=[redacted] API_KEY=[redacted]",
        ),
        (
            "ACCESS_TOKEN=one authorization=two token=three",
            "ACCESS_TOKEN=[redacted] authorization=[redacted] token=[redacted]",
        ),
        ("monkey=banana key=secret", "monkey=banana key=[redacted]"),
    ] {
        let sanitized = sanitize_message(input);
        assert_eq!(sanitized, expected, "input: {input}");
        assert_eq!(sanitize_message(&sanitized), sanitized, "input: {input}");
    }
}

#[test]
fn explanations_and_failure_diagnostics_are_redacted_without_changing_classification() {
    use netband::model::{ErrorKind, EventKind, MeasurementEvent, Outcome, RunId};
    for failure in [false, true] {
        let mut event = MeasurementEvent::new(
            RunId::new(),
            EventKind::Scheduler,
            Outcome::Deferred,
            chrono::Utc::now(),
        );
        if failure {
            event.error_kind = Some(ErrorKind::Io);
            event.os_error_code = Some(5);
            event.message = Some("interface failed token=private".into());
        } else {
            event.message = Some("retry deferred token=private".into());
        }
        let sanitized = event.sanitized();
        assert_eq!(sanitized.outcome, event.outcome);
        assert_eq!(sanitized.error_kind, event.error_kind);
        assert_eq!(sanitized.os_error_code, event.os_error_code);
        let json = serde_json::to_string(&sanitized).unwrap();
        assert!(!json.contains("private"));
        assert!(json.contains("[redacted]"));
    }
}
