# Dipper
> 여러 대형 DB에서 필요한 데이터만 작은 그릇에 담아 빠르게 검색·분석하는 도구

Dipper picks only the data you need out of multiple large databases and moves it into a small store, so you can search and analyze it quickly on your own PC or in the cloud.

## Why Dipper

You often want just a slice of data that lives across several large databases. Querying those originals every time is slow, and it puts a steady burden on systems that were not meant to answer the same narrow questions over and over. Dipper takes that slice once and keeps it in a small store you control, so later search and analysis stay fast and stay off the source systems.

## Key Features

- Extract only the data you need from multiple large databases
- Keep that slice in a lightweight store, on your own PC or in the cloud
- Search and analyze the small store quickly
- Source databases and their indexes are never modified (read-only)
- Usable without SQL

## How It Works

1. **Select** the slice of data you need from one or more large databases.
2. **Dip** a copy of that slice into a small store.
3. **Search & analyze** the copy on your own PC or in the cloud.

```mermaid
flowchart LR
  select[Select] --> dip[Dip into a small store]
  dip --> search[Search and analyze]
```

## Use Cases

- Pull a narrow subset from several large databases and explore it locally, without waiting on the originals each time.
- Build a small working set for repeated questions, so day-to-day analysis does not keep hitting the source systems.
- Share a compact copy in the cloud so collaborators can search the same slice without direct access to the source databases.
- Gather and look through the records you need even if you do not write SQL.

## Project Status

Dipper is in early development. There is no installable release yet. Installation and usage instructions will be added later.

`dipper dip` and `dipper ontology pull` can print one JSON document with `--format json`. The field list is in [docs/CLI_JSON.md](docs/CLI_JSON.md).

## Contributing

Issues and pull requests are welcome. If you have a question, an idea, or a fix, please open an issue or a pull request.
