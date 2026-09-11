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

For field definitions, availability rules, units, and record relationships, use the
[data-format reference](../data-format.md). The scenarios illustrate these together:

| Example | Interpretation |
| --- | --- |
| Ping records (1–3) | One record per attempt; an unsent probe has no loss percentage |
| Loaded ping (5) | `load_run_id` links to the bandwidth attempt; `load_phase` identifies download activity |
| Successful bandwidth (6) | `elapsed_ms` is 20,200 ms; the two active transfer windows total 20,000 ms; upload retransmitted bytes demonstrates measured zero |
| Upload failure (7–8) | `request_direction=upload` and `request_stage=connect` identify the failure; `request_url` names its endpoint and `server_name` the logical server; `run_id` joins the result |
| Interrupted bandwidth (9–10) | Download measurements remain available; missing upload measurements are unavailable, not zero |
| Locate failure (11–13) | Shared discovery has no request direction; the attempt has 100 ms elapsed time despite producing no transfer measurements |
| Scheduler records (4, 13–14) | Decision explanations use `error_message`; measurement fields, including `elapsed_ms`, are null |

For easier inspection without changing the JSONL file:

```sh
jq . docs/examples/console.jsonl
```
