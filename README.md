# river-query

`river-query-api`: unified HTTP query layer (axum, filter DSL) over ClickHouse and VictoriaMetrics. Port `8080` (`RIVER_API_PORT`).

`openapi.json` at the repo root is the published API contract; keep it in sync with `GET /openapi.json` when routes change.

```bash
mise run ci:cargo-test
```

Specs and agent context live in [river-spec](https://github.com/bsterligov/river-spec).
