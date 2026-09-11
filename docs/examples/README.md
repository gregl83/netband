# JSONL output examples

[console.jsonl](console.jsonl) contains 14 synthetic records illustrating all four
current event kinds. These are independent example scenarios, not captured network
measurements or one continuous session. Names, addresses, identifiers, and values are
illustrative. Every record includes all schema fields; unavailable values are null.

| Lines | Scenario |
| --- | --- |
| 1–3 | Successful ping, timed-out ping, and an unsent permission-denied ping |
| 4–6 | Scheduled bandwidth start, a ping during download, and a successful two-direction test with TCP metrics |
| 7–8 | Upload connection failure and its partial bandwidth result retaining download measurements |
| 9–10 | Cancellation during upload and its bandwidth result retaining download measurements |
| 11–13 | Locate HTTP 429 with Retry-After, a bandwidth result with no measurements, and the resulting scheduler deferral |
| 14 | Scheduler suppression after the daily start limit |

The populated `connection_details.wifi` object on line 5 illustrates what a future
collector could supply. **Current collectors leave connection details unavailable.**
The object demonstrates the extension contract; it does not claim Wi-Fi collection is
implemented. Other fields illustrate current output behavior.

Use `event_kind` to distinguish measurements from operational events. Each ping is
one complete `ping_probe` row. Request failures join their single bandwidth result
by `run_id`; `load_run_id` joins the loaded ping to its bandwidth result. Count only
bandwidth rows when counting bandwidth tests. Missing upload values in partial or
cancelled results are unavailable, not measured zero throughput. The successful
upload's retransmitted-byte counter demonstrates a genuine zero.

`server_name` identifies the logical measurement server; `request_url` identifies a
failed request's endpoint. The redacted query marker illustrates sanitization.
Scheduler decisions currently use `error_message` for explanatory text, including
non-error decisions; this example does not introduce the proposed scheduler-action
field. `request_direction=upload` identifies the upload connection failure even
though its `request_stage` is `connect`; Locate failures have no direction.

Read the [data-format contract](../data-format.md) for units and field meanings.
For easier inspection without changing the JSONL file:

```sh
jq . docs/examples/console.jsonl
```
