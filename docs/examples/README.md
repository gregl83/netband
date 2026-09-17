# JSONL output examples

[console.jsonl](console.jsonl) is a synthetic session showing all six event kinds.
It includes successful, timed-out and unsent pings; a loaded ping overlapping a
bandwidth attempt; successful and partial bandwidth results; upload cancellation;
Locate rate limiting; and scheduler deferral and suppression. These are illustrative
records, not captured network measurements. Every record includes all 69 fields.

The matching [CSV session fixture](../../tests/fixtures/v1-events.csv) contains the
same 32 records in the same order. Tests keep both representations aligned and
verify lifecycle ordering and parent relationships. The separate
[serialization fixture](../../tests/fixtures/serialization-edge-events.csv) covers
CSV escaping and diagnostic redaction; it is not a complete session.

Follow `event_sequence` for emission order. The root session start records command,
version and PID. Child starts precede their results, and every child record points
back to that session through `parent_run_id`. Every record shares its `root_run_id`.
The rate-limit scheduler decision belongs to the affected bandwidth run and appears
before its finish; general scheduler decisions belong to the session. Ping-round grouping uses `run_id`;
`load_run_id` separately identifies concurrent bandwidth work. Request IDs join
retained directional measurements to any diagnostics from the same request.

Lifecycle records carry no measurement values. Child starts include their known
interface, bandwidth provider/trigger, or ping load snapshot; discovered addresses,
server selection, and accounting remain unavailable until results. Normal scheduler explanations use
`message`, `scheduler_action`, and `scheduler_reason`, leaving `error_kind` and `os_error_code` empty. Run finishes retain the
operation outcome; the root session ends with orderly cancellation.

The populated `connection_details.wifi` object illustrates what a future collector
could supply. **Current collectors leave connection details unavailable.**

For definitions, availability, units and relationships, see the
[data-format reference](../data-format.md). For easier inspection:

```sh
jq . docs/examples/console.jsonl
```

Columns follow the same family order as the data contract. `message` contains either
a normal explanation or a failure diagnostic; structured outcomes and error kinds
determine its context. Ping loss is derived from `ping_packets_sent` and
`ping_packets_received`. Request retry deadlines and scheduler eligibility deadlines
occupy separate columns even when their timestamps happen to match.

Reservation accounting appears on bandwidth summaries and provider scheduler decisions.
Bandwidth summaries retain the admission date/count and reservation flag; failed
discovery has a false flag and no count. Request failures carry no accounting snapshot.

`scheduled_at_utc` records planned execution only; manual bandwidth summaries leave
it empty. `requested_at_utc` records the original bandwidth request separately.
Periodic pings retain planned ticker times; one-shot pings have no planned time.
