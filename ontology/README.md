# Ontology files

`dipper ontology pull` rewrites only `extracted/<source>/<owner>.<table>.json`.
It does not create, change, or delete `overlay/`.

`overlay/<source>/<owner>.<table>.json` holds human meaning and unit notes, plus estimated `relations`.
It cannot replace declared `foreign_keys`. `dipper ontology validate` rejects that field.
`dipper ontology render` writes `md/<source>/<owner>.<table>.md` and `md/<source>/_index.md` from the extracted document plus that overlay.
Foreign keys and relations become Obsidian links of the form `[[owner.table]]`.
`dipper ontology validate` rejects a document that does not match the schema.

The extractor leaves `meaning` and `unit` null and sets both statuses to `unknown`.
A new overlay uses `draft` for a meaning that still needs review. `estimated` remains valid for older overlays.
A `draft` relation must include evidence. `confirmed` is optional evidence.

`dipper ontology check-relation` prints overlap counts for one candidate relationship.
It does not print row values. `--format json` is described in [docs/CLI_JSON.md](../docs/CLI_JSON.md).

External assistants follow [skills/dipper/SKILL.md](../skills/dipper/SKILL.md).
The thin adapters are `.claude/skills/dipper/SKILL.md`, `.cursor/rules/dipper.mdc`, and `AGENTS.md`.
