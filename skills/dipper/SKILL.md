# Dipper

Use this skill when a person asks to pull a Dipper ontology, fill in meanings, find relationships that are not declared foreign keys, refresh Obsidian notes, or load a time range into the local store.

Run only the Dipper CLI. Do not open a database driver yourself.

## Environment

Start with `dipper check --env-only`. It reports `executable`, `config`, `oracle_feature`, and `instant_client` without opening a database. The report names environment variables and does not print their values. Fix a `fail` item before pulling data. Oracle setup is `docs/ORACLE_SETUP.md`.

## Find candidates

Do not guess table names and `pull` until one works. When a foreign key is missing:

1. Run `dipper ontology find <source> --column <KEY> [--owner OWNER] --format json` to list tables that have that column.
2. Run `dipper ontology list <source> --owner OWNER [--name-like PAT] --format json` for name patterns such as `%NODE%` or `%MST%`.
3. `pull` only the tables you still need.

`list` and `find` read the dictionary only. They do not return row values.

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
6. Draw the relationship diagram with `dipper ontology diagram <source> [--root OWNER.TABLE] [--depth N] [--group OWNER.TABLE] [--confirmed-only] [--mmd]`.
   It reads only local JSON and writes `ontology/diagrams/<source>/<name>.svg`.
   Confirmed relations and foreign keys are solid lines. Draft relations are dashed.
   Optional `kind` is `reference` or `rollup`. A rollup requires `label`.
   Optional `condition` and `label` are short notes shown on the line.
   Do not fill `condition` or `label` from row values. Take them only from a database comment or a person's explanation.
   Do not change `kind`, `condition`, or `label` on a `confirmed` relation unless the person said to.

## Period load

Run:

`dipper dip <name> --from <YYYYMMDD|...> --to <YYYYMMDD|...> --time-column <COL> --yes --format json`

Add `--refresh` only when the person asks to reload a range that is already stored.

Explain the JSON in the person's language: `period`, `held`, `fetch`, `COUNT`, `ignored_filters`, `nothing_to_fetch`, `rows_loaded`, `error` (`code`, `retryable`, `message`), and the exit code (`0` success, `1` failure, `2` usage).

If `error.retryable` is true, wait 5 seconds and run that command once more. If it fails again, tell the person. Never retry `object_not_found`. Dipper itself does not retry.

## Never do this

- Do not put source row values, DuckDB row samples, passwords, connect strings, or database usernames into the model context, the conversation, a file, or an overlay. Config examples use only the variable names `DIPPER_ORA_USER` and `DIPPER_ORA_PASSWORD`.
- Do not bypass `GuardedSelect`, `READ_ONLY`, or binds, and do not connect with another driver. The allowed commands are `check`, `ontology list`, `ontology find`, `ontology pull`, `ontology validate`, `ontology render`, `ontology diagram`, `ontology check-relation`, and `dip`.
- Do not add `DIPPER_AI_*` settings, API keys, or cost limits to the Dipper source.
- Do not merge a pull request unless the person explicitly approves it.
