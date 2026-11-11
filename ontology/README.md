# Ontology files

`dipper ontology pull` rewrites only `extracted/<source>/<owner>.<table>.json`.
It does not create, change, or delete `overlay/`.

`overlay/<source>/<owner>.<table>.json` holds human meaning and unit notes.
`dipper ontology render` writes `md/` from the extracted document plus that overlay.
`dipper ontology validate` rejects a document that does not match the schema.

The extractor leaves `meaning` and `unit` null and sets both statuses to `unknown`.
