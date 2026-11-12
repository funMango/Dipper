# JSON output

`dipper dip` and `dipper ontology pull` accept `--format json`. Omitting `--format` keeps the existing table or path list. The flag is not global: `render` and `validate` do not take it.

`--format json` writes one JSON document to stdout and a final newline. Progress, `preview:`, `note:`, `ignored filter:`, `nothing to fetch:`, `--log-sql`, and `dipper:` stay on stderr. A clap usage error (exit 2), including an unknown format such as `--format xml`, writes no JSON.

`schema_version` is the integer `1`. New fields may be added without changing it. Renaming, removing, or changing the meaning of a field bumps the version.

Timestamps use `YYYY-MM-DDTHH:MM:SS` with no timezone. A period range is half-open: `start` inclusive, `end` exclusive. An exact instant `T` (`--from T --to T`) is the one-second ledger interval `[T, T+1s)` and `"exact": true`.

JSON does not contain source row values, DuckDB samples, passwords, Oracle connect strings, or Oracle user names. An ignored config filter value is included because it comes from the config file. If an error contains the Oracle user (ASCII case-insensitive), password, full connect string, or the host, port, or service taken from that connect string, the whole message is replaced with `details withheld`. An `ORA-#####` or `DPI-####` code is kept only when that code does not itself contain one of those values. If every code in an already-withheld driver sentence contains a secret, that sentence becomes `driver error (driver message hidden because it contained the password)` (a password of `1047` does not leave `DPI-1047`). A code that does not contain a secret is left in the sentence. Percent-encoded connect text is not decoded.

Redaction is not one function call. `redact_known_values` is the shared string rule. `run_one` and `run_period_one` apply it through `redact_with_config` to the outcome, and `_dipper_runs.error` is that same withheld text. The public `DipOutcome.result` does not keep the original message. `hide_text` applies the same rule again for JSON `dips[].error` and for the table error column in `write_report`. Pre-run dip failures go through `finish_error` → `redact_error`, and `ontology pull` failures go through `redact_ontology_error` → `redact_error`, before the JSON document and before the error returns to `main` for the `dipper:` line. A failed dip's `dipper:` line is `dip failed: …` and does not include the per-dip message; that message is in the table or JSON and in `_dipper_runs.error`.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | The command finished. `ok` is true. |
| 1 | Config, execution, or dip failure. `ok` is false. |
| 2 | Usage, an unknown dip, or a command that is not implemented. Clap usage errors print no JSON. |

## `dip`

Top-level fields:

| Field | Meaning |
| --- | --- |
| `schema_version` | Integer `1`. |
| `command` | `"dip"`. |
| `ok` | True when the process will exit 0. |
| `exit_code` | The process exit code. |
| `period` | Null when `--from`/`--to` were not used. Otherwise the object below. |
| `dips` | One object per selected dip, in config order. Several dips share this one document. |
| `summary` | Counts of those dip objects. |
| `error` | `{"message": "..."}` only when the command stopped before running dips. Otherwise null. A failed dip is listed in `dips` and leaves this field null. |

`period`:

| Field | Meaning |
| --- | --- |
| `time_column` | CLI `--time-column`. |
| `from`, `to` | CLI text, unchanged. |
| `exact` | True when `--from` and `--to` are the same timestamp. |
| `start`, `end` | Resolved half-open interval. Exact instants use `[T, T+1s)`. |
| `mode` | `fill`, `refresh`, or `replace`. |

`dips[]`:

| Field | Meaning |
| --- | --- |
| `name`, `source` | Dip name and source name. |
| `status` | `ok`, `failed`, `nothing_to_fetch`, or `not_run`. |
| `refresh` | True when this run used `--refresh`. |
| `ignored_filters` | Config filters on the time column that `--from`/`--to` replaced for this run. `op` is the filter name (`eq`, `gte`, …). `value` is the config value as JSON, or null. |
| `held` | Merged ledger ranges already stored, each `{start, end}`. |
| `fetch` | Missing ranges and their preview `count`. |
| `rows_loaded` | Rows written. `0` for `nothing_to_fetch` and `not_run`. |
| `seconds` | Elapsed seconds, or null when the dip did not run. |
| `warnings` | Preflight warning count. |
| `error` | Failure message, or null. |

Without a period, `held`, `fetch`, and `ignored_filters` are empty and `refresh` is false.

`summary`: `total`, `ok`, `failed`, `not_run`, `nothing_to_fetch`.

`--format json` with a period and without `--yes` does not prompt. It exits 1 before reading the config file. The message is `--format json needs --yes for a period load`. The 31-day, 1,000,000-row, missing-ledger, and zero-row `--replace-table` checks still apply when `--yes` is set.

Example after August is already loaded and the request is 15 Aug through 30 Sep:

```json
{"schema_version":1,"command":"dip","ok":true,"exit_code":0,"period":{"time_column":"TS","from":"260815","to":"260930","exact":false,"start":"2026-08-15T00:00:00","end":"2026-10-01T00:00:00","mode":"fill"},"dips":[{"name":"events","source":"local","status":"ok","refresh":false,"ignored_filters":[],"held":[{"start":"2026-08-01T00:00:00","end":"2026-09-01T00:00:00"}],"fetch":[{"start":"2026-09-01T00:00:00","end":"2026-10-01T00:00:00","count":2}],"rows_loaded":2,"seconds":0.1,"warnings":0,"error":null}],"summary":{"total":1,"ok":1,"failed":0,"not_run":0,"nothing_to_fetch":0},"error":null}
```

`seconds` in a real document is the measured duration, not a rounded example.

## `ontology pull`

| Field | Meaning |
| --- | --- |
| `schema_version` | Integer `1`. |
| `command` | `"ontology.pull"`. |
| `ok`, `exit_code` | Same meaning as `dip`. |
| `source` | The source name argument. |
| `files` | Extracted paths in the same order as table-mode stdout. |
| `error` | `{"message": "..."}` on failure, otherwise null. |

```json
{"schema_version":1,"command":"ontology.pull","ok":true,"exit_code":0,"source":"sample","files":["ontology/extracted/sample/main.EVENTS.json"],"error":null}
```
