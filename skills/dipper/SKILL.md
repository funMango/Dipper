# Dipper

Use this skill when a person asks to pull a Dipper ontology, fill in meanings, find relationships that are not declared foreign keys, refresh Obsidian notes, or load a time range into the local store.

Run only the Dipper CLI. Do not open a database driver yourself.

## Ontology and relationships

1. Run `dipper ontology pull <source> [--table OWNER.TABLE] --format json`.
   Then read `ontology/extracted/<source>/<owner>.<table>.json`.
2. Write or update `ontology/overlay/<source>/<owner>.<table>.json`.
   Never edit `ontology/extracted/`.
   When a database comment already states the meaning, set `meaning_status` to `"draft"`.
   Leave unclear abbreviations unknown until the person explains them. `JYWK_CNT`, `VS`, and `XC` stay `unknown`. Do not guess.
3. Read `foreign_keys` first. Those links are already declared.
   For tables with no declared foreign key, compare column names, data types, length or precision, and database comments.
   When a candidate needs a value-overlap check, run:

   `dipper ontology check-relation <source> --from-table OWNER.TABLE --from-column COL --to-table OWNER.TABLE --to-column COL --format json`

   Repeat `--from-column` and `--to-column` together for a composite key.
   The command returns counts only. It cannot show row values, and you must not ask for them.
   Record the candidate in `overlay.relations` with `status` `"draft"` and `evidence` that cites the column comparison and the overlap counts.
4. Show each `draft` meaning and relationship to the person.
   Change `status` to `"confirmed"` only after they accept that item.
5. Run `dipper ontology validate`, then `dipper ontology render`.
   Render writes Obsidian notes under `ontology/md/`, including `[[owner.table]]` links and `_index.md`.

## Period load

Run:

`dipper dip <name> --from <YYYYMMDD|...> --to <YYYYMMDD|...> --time-column <COL> --yes --format json`

Add `--refresh` only when the person asks to reload a range that is already stored.

Explain the JSON in the person's language: `period`, `held`, `fetch`, `COUNT`, `ignored_filters`, `nothing_to_fetch`, `rows_loaded`, `error`, and the exit code (`0` success, `1` failure, `2` usage).

## Never do this

- Do not put source row values, DuckDB row samples, passwords, connect strings, or database usernames into the model context, the conversation, a file, or an overlay. Config examples use only the variable names `DIPPER_ORA_USER` and `DIPPER_ORA_PASSWORD`.
- Do not bypass `GuardedSelect`, `READ_ONLY`, or binds, and do not connect with another driver. The allowed commands are `ontology pull`, `ontology validate`, `ontology render`, `ontology check-relation`, and `dip`.
- Do not add `DIPPER_AI_*` settings, API keys, or cost limits to the Dipper source.
- Do not merge a pull request unless the person explicitly approves it.
