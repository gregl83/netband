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
