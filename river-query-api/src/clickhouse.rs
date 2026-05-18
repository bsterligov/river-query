use anyhow::{bail, Result};
use chrono::DateTime;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use utoipa::ToSchema;

use crate::filter;

// --- Public output DTOs ---

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LogRow {
    pub timestamp: String,
    pub severity: String,
    pub service: String,
    pub body: String,
    pub trace_id: String,
    pub severity_number: i64,
    pub span_id: String,
    pub attributes: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HistogramBucket {
    pub bucket: String,
    pub count: u64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct FacetValue {
    pub value: String,
    pub count: u64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct FacetField {
    pub field: String,
    pub values: Vec<FacetValue>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SpanEvent {
    pub name: String,
    pub timestamp: String,
    pub attributes: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SpanLink {
    pub trace_id: String,
    pub span_id: String,
    pub attributes: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct Span {
    pub span_id: String,
    pub parent_span_id: String,
    pub service: String,
    pub operation: String,
    pub start_time: String,
    pub end_time: String,
    pub duration_ms: f64,
    pub status_code: i64,
    pub attributes: serde_json::Value,
    pub events: Vec<SpanEvent>,
    pub links: Vec<SpanLink>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TraceGroup {
    pub trace_id: String,
    pub spans: Vec<Span>,
}

// --- Private typed ClickHouse row structs ---

#[derive(Debug, Deserialize)]
struct ChLogRow {
    // ClickHouse DateTime64 arrives as "YYYY-MM-DD HH:MM:SS.nnn" string in real
    // responses; some test fixtures use an integer epoch — accept both.
    timestamp: serde_json::Value,
    #[serde(default)]
    severity_text: String,
    #[serde(default)]
    service_name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    trace_id: String,
    #[serde(default)]
    severity_number: i64,
    #[serde(default)]
    span_id: String,
    #[serde(default)]
    attributes: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct ChHistogramRow {
    bucket: String,
    count: u64,
}

#[derive(Debug, Deserialize)]
struct ChFacetValueRow {
    value: String,
    count: u64,
}

#[derive(Debug, Deserialize)]
struct ChSpanRow {
    #[serde(default)]
    trace_id: Option<String>,
    span_id: String,
    parent_span_id: String,
    service_name: String,
    operation_name: String,
    start_time_unix_nano: u64,
    end_time_unix_nano: u64,
    duration_ns: u64,
    status_code: i64,
    #[serde(default)]
    attributes: serde_json::Value,
    #[serde(rename = "Events.Name", default)]
    events_name: Vec<serde_json::Value>,
    #[serde(rename = "Events.Timestamp", default)]
    events_timestamp: Vec<serde_json::Value>,
    #[serde(rename = "Events.Attributes", default)]
    events_attributes: Vec<serde_json::Value>,
    #[serde(rename = "Links.TraceId", default)]
    links_trace_id: Vec<serde_json::Value>,
    #[serde(rename = "Links.SpanId", default)]
    links_span_id: Vec<serde_json::Value>,
    #[serde(rename = "Links.Attributes", default)]
    links_attributes: Vec<serde_json::Value>,
}

// --- Conversions from CH row structs to output DTOs ---

impl From<ChLogRow> for LogRow {
    fn from(row: ChLogRow) -> Self {
        let timestamp = match &row.timestamp {
            serde_json::Value::String(s) => ch_datetime64_to_rfc3339(s),
            serde_json::Value::Number(n) => ns_to_rfc3339(n.as_u64().unwrap_or(0)),
            _ => String::new(),
        };
        LogRow {
            timestamp,
            severity: row.severity_text,
            service: row.service_name,
            body: row.body,
            trace_id: row.trace_id,
            severity_number: row.severity_number,
            span_id: row.span_id,
            attributes: parse_attributes(&row.attributes),
        }
    }
}

impl From<ChHistogramRow> for HistogramBucket {
    fn from(row: ChHistogramRow) -> Self {
        HistogramBucket {
            bucket: row.bucket,
            count: row.count,
        }
    }
}

impl From<ChFacetValueRow> for FacetValue {
    fn from(row: ChFacetValueRow) -> Self {
        FacetValue {
            value: row.value,
            count: row.count,
        }
    }
}

impl From<ChSpanRow> for Span {
    fn from(row: ChSpanRow) -> Self {
        let events = row
            .events_name
            .into_iter()
            .zip(row.events_timestamp)
            .zip(row.events_attributes)
            .map(|((name, ts), attrs)| SpanEvent {
                name: name.as_str().unwrap_or_default().to_string(),
                timestamp: ns_to_rfc3339(ts.as_u64().unwrap_or(0)),
                attributes: attrs,
            })
            .collect();

        let links = row
            .links_trace_id
            .into_iter()
            .zip(row.links_span_id)
            .zip(row.links_attributes)
            .map(|((tid, sid), attrs)| SpanLink {
                trace_id: tid.as_str().unwrap_or_default().to_string(),
                span_id: sid.as_str().unwrap_or_default().to_string(),
                attributes: attrs,
            })
            .collect();

        Span {
            span_id: row.span_id,
            parent_span_id: row.parent_span_id,
            service: row.service_name,
            operation: row.operation_name,
            start_time: ns_to_rfc3339(row.start_time_unix_nano),
            end_time: ns_to_rfc3339(row.end_time_unix_nano),
            duration_ms: row.duration_ns as f64 / 1_000_000.0,
            status_code: row.status_code,
            attributes: parse_attributes(&row.attributes),
            events,
            links,
        }
    }
}

// --- Reader ---

pub struct Reader {
    client: reqwest::Client,
    base_url: String,
    db: String,
    user: String,
    password: String,
}

impl Reader {
    pub fn new(
        client: reqwest::Client,
        base_url: String,
        db: String,
        user: String,
        password: String,
    ) -> Self {
        Reader {
            client,
            base_url,
            db,
            user,
            password,
        }
    }

    pub async fn query_logs(
        &self,
        filter: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        limit: u32,
    ) -> Result<Vec<LogRow>> {
        let where_clause =
            build_where_clause(filter, from, to, filter::to_clickhouse_logs, "timestamp")?;

        let sql = format!(
            "SELECT timestamp, severity_text, service_name, body, trace_id, \
             severity_number, span_id, attributes \
             FROM logs{where_clause} \
             ORDER BY timestamp DESC \
             LIMIT {limit} \
             FORMAT JSONEachRow"
        );

        let rows: Vec<ChLogRow> = self.query_typed(&sql).await?;
        Ok(rows.into_iter().map(LogRow::from).collect())
    }

    pub async fn query_logs_histogram(
        &self,
        filter: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        step_secs: Option<u64>,
    ) -> Result<Vec<HistogramBucket>> {
        let where_clause =
            build_where_clause(filter, from, to, filter::to_clickhouse_logs, "timestamp")?;

        let step = match step_secs {
            Some(s) => s,
            None => {
                let range_secs = range_secs(from, to)?;
                auto_step(range_secs)
            }
        };

        let sql = format!(
            "SELECT toStartOfInterval(timestamp, INTERVAL {step} SECOND) AS bucket, \
             count() AS count \
             FROM logs{where_clause} \
             GROUP BY bucket \
             ORDER BY bucket ASC \
             FORMAT JSONEachRow"
        );

        let rows: Vec<ChHistogramRow> = self.query_typed(&sql).await?;

        // Build a map from bucket label → count for gap-filling below.
        let mut counts: std::collections::HashMap<String, u64> =
            std::collections::HashMap::with_capacity(rows.len());
        for row in &rows {
            counts.insert(row.bucket.clone(), row.count);
        }

        // Generate every expected bucket from `from` to `to` and fill gaps with 0.
        // This ensures the chart always shows the full time range, not just
        // buckets that happen to contain at least one log.
        let from_secs = from
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| {
                // Align to the step boundary (floor division).
                let ts = dt.timestamp() as u64;
                ts - (ts % step)
            })
            .unwrap_or(0);
        let to_secs = to
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp() as u64)
            .unwrap_or(from_secs);

        let mut out = Vec::new();
        let mut t = from_secs;
        while t <= to_secs {
            let label = chrono::DateTime::from_timestamp(t as i64, 0)
                .map(|dt: chrono::DateTime<chrono::Utc>| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default();
            let count = counts.get(&label).copied().unwrap_or(0);
            out.push(HistogramBucket {
                bucket: label,
                count,
            });
            t = t.saturating_add(step);
        }

        Ok(out)
    }

    async fn query_facets(
        &self,
        table: &str,
        fields: &[&str],
        where_clause: &str,
    ) -> Vec<FacetField> {
        let mut result = Vec::with_capacity(fields.len());
        for &field in fields {
            let sql = format!(
                "SELECT {field} AS value, count() AS count \
                 FROM {table}{where_clause} \
                 GROUP BY {field} \
                 ORDER BY count DESC \
                 LIMIT 20 \
                 FORMAT JSONEachRow"
            );
            let values = self
                .query_typed::<ChFacetValueRow>(&sql)
                .await
                .map(|rows| rows.into_iter().map(FacetValue::from).collect())
                .unwrap_or_default();
            result.push(FacetField {
                field: field.to_string(),
                values,
            });
        }
        result
    }

    pub async fn query_logs_facets(
        &self,
        filter: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<FacetField>> {
        let where_clause =
            build_where_clause(filter, from, to, filter::to_clickhouse_logs, "timestamp")?;
        Ok(self
            .query_facets("logs", &["service_name", "severity_text"], &where_clause)
            .await)
    }

    pub async fn query_traces_facets(
        &self,
        filter: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<FacetField>> {
        let where_clause = build_where_clause(
            filter,
            from,
            to,
            filter::to_clickhouse_traces,
            "start_time_unix_nano",
        )?;
        Ok(self
            .query_facets("traces", &["service_name", "operation_name"], &where_clause)
            .await)
    }

    pub async fn query_traces(
        &self,
        filter: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        limit: u32,
    ) -> Result<Vec<TraceGroup>> {
        let where_clause = build_where_clause(
            filter,
            from,
            to,
            filter::to_clickhouse_traces,
            "start_time_unix_nano",
        )?;

        let sql = format!(
            "SELECT trace_id, span_id, parent_span_id, service_name, operation_name, \
             toUnixTimestamp64Nano(start_time_unix_nano) AS start_time_unix_nano, \
             toUnixTimestamp64Nano(end_time_unix_nano) AS end_time_unix_nano, \
             duration_ns, status_code, attributes, \
             `Events.Name`, \
             arrayMap(t -> toUnixTimestamp64Nano(t), `Events.Timestamp`) AS `Events.Timestamp`, \
             `Events.Attributes`, \
             `Links.TraceId`, `Links.SpanId`, `Links.Attributes` \
             FROM (SELECT * FROM traces{where_clause} ORDER BY start_time_unix_nano DESC LIMIT {limit}) \
             FORMAT JSONEachRow"
        );

        let rows: Vec<ChSpanRow> = self.query_typed(&sql).await?;
        let mut groups: std::collections::HashMap<String, Vec<Span>> =
            std::collections::HashMap::new();

        for row in rows {
            let trace_id = row.trace_id.clone().unwrap_or_default();
            groups.entry(trace_id).or_default().push(Span::from(row));
        }

        let mut result: Vec<TraceGroup> = groups
            .into_iter()
            .map(|(trace_id, spans)| TraceGroup { trace_id, spans })
            .collect();
        result.sort_by(|a, b| a.trace_id.cmp(&b.trace_id));
        Ok(result)
    }

    pub async fn query_trace(&self, trace_id: &str) -> Result<Vec<Span>> {
        let escaped = trace_id.replace('\'', "\\'");
        let sql = format!(
            "SELECT span_id, parent_span_id, service_name, operation_name, \
             toUnixTimestamp64Nano(start_time_unix_nano) AS start_time_unix_nano, \
             toUnixTimestamp64Nano(end_time_unix_nano) AS end_time_unix_nano, \
             duration_ns, status_code, attributes, \
             `Events.Name`, \
             arrayMap(t -> toUnixTimestamp64Nano(t), `Events.Timestamp`) AS `Events.Timestamp`, \
             `Events.Attributes`, \
             `Links.TraceId`, `Links.SpanId`, `Links.Attributes` \
             FROM traces \
             WHERE trace_id = '{escaped}' \
             ORDER BY start_time_unix_nano ASC \
             FORMAT JSONEachRow"
        );

        let rows: Vec<ChSpanRow> = self.query_typed(&sql).await?;
        Ok(rows.into_iter().map(Span::from).collect())
    }

    async fn query_typed<T: DeserializeOwned>(&self, sql: &str) -> Result<Vec<T>> {
        let resp = self
            .client
            .get(&self.base_url)
            .query(&[
                ("query", sql),
                ("user", self.user.as_str()),
                ("password", self.password.as_str()),
                ("database", self.db.as_str()),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("(error reading response body: {e})"));
            bail!("clickhouse query failed status={status}: {body}");
        }

        let text = resp.text().await?;
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).map_err(Into::into))
            .collect()
    }
}

fn parse_attributes(val: &serde_json::Value) -> serde_json::Value {
    if let Some(s) = val.as_str() {
        serde_json::from_str(s).unwrap_or(serde_json::Value::Object(Default::default()))
    } else if val.is_object() {
        val.clone()
    } else {
        serde_json::Value::Object(Default::default())
    }
}

const STEP_LADDER: &[u64] = &[60, 300, 900, 3600, 21600, 86400];

fn auto_step(range_secs: u64) -> u64 {
    let target = range_secs / 30;
    *STEP_LADDER.iter().find(|&&s| s >= target).unwrap_or(&86400)
}

fn range_secs(from: Option<&str>, to: Option<&str>) -> Result<u64> {
    let from_ns = from.map(rfc3339_to_ns).transpose()?.unwrap_or(0);
    let to_ns = to.map(rfc3339_to_ns).transpose()?.unwrap_or(0);
    Ok(to_ns.saturating_sub(from_ns) / 1_000_000_000)
}

fn build_where_clause(
    filter: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    filter_to_sql: fn(&filter::Expr) -> Result<String, String>,
    time_field: &str,
) -> Result<String> {
    let mut clauses: Vec<String> = Vec::new();

    if let Some(f) = filter {
        let expr = filter::parse(f).map_err(|e| anyhow::anyhow!("filter parse error: {e}"))?;
        if let Some(expr) = expr {
            clauses.push(
                filter_to_sql(&expr).map_err(|e| anyhow::anyhow!("filter translate error: {e}"))?,
            );
        }
    }
    if let Some(ts) = from {
        clauses.push(format!(
            "{time_field} >= toDateTime64('{}', 9)",
            rfc3339_to_ch(ts)?
        ));
    }
    if let Some(ts) = to {
        clauses.push(format!(
            "{time_field} <= toDateTime64('{}', 9)",
            rfc3339_to_ch(ts)?
        ));
    }

    if clauses.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!(" WHERE {}", clauses.join(" AND ")))
    }
}

fn rfc3339_to_ch(s: &str) -> Result<String> {
    let dt = DateTime::parse_from_rfc3339(s)
        .map_err(|e| anyhow::anyhow!("invalid RFC 3339 timestamp '{s}': {e}"))?;
    Ok(dt.format("%Y-%m-%d %H:%M:%S%.9f").to_string())
}

fn rfc3339_to_ns(s: &str) -> Result<u64> {
    let dt = DateTime::parse_from_rfc3339(s)
        .map_err(|e| anyhow::anyhow!("invalid RFC 3339 timestamp '{s}': {e}"))?;
    Ok(dt.timestamp_nanos_opt().unwrap_or(0) as u64)
}

fn ns_to_rfc3339(ns: u64) -> String {
    let secs = (ns / 1_000_000_000) as i64;
    let nanos = (ns % 1_000_000_000) as u32;
    DateTime::from_timestamp(secs, nanos)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| format!("(invalid timestamp ns={ns})"))
}

// ClickHouse DateTime64(9) JSONEachRow format: "YYYY-MM-DD HH:MM:SS.nnnnnnnnn"
fn ch_datetime64_to_rfc3339(s: &str) -> String {
    // Parse "2026-05-14 17:49:11.400803100" into RFC 3339
    let s = s.trim();
    if s.is_empty() {
        return String::new();
    }
    // Replace space separator with T and ensure timezone suffix
    let iso = if s.contains('T') {
        format!("{s}+00:00")
    } else {
        format!("{}+00:00", s.replacen(' ', "T", 1))
    };
    // chrono can parse "2026-05-14T17:49:11.400803100+00:00"
    chrono::DateTime::parse_from_rfc3339(&iso)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|_| s.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, query_param_contains};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_reader(url: String) -> Reader {
        Reader::new(
            reqwest::Client::new(),
            url,
            "river".to_string(),
            "river".to_string(),
            "river".to_string(),
        )
    }

    async fn get_server(status: u16, body: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(&server)
            .await;
        server
    }

    #[test]
    fn rfc3339_roundtrip() {
        let input = "2024-01-01T00:00:00Z";
        let ns = rfc3339_to_ns(input).unwrap();
        let back = ns_to_rfc3339(ns);
        assert!(back.starts_with("2024-01-01T00:00:00"));
    }

    #[test]
    fn rfc3339_to_ns_rejects_invalid() {
        assert!(rfc3339_to_ns("not-a-date").is_err());
    }

    #[test]
    fn ch_datetime64_parses_clickhouse_format() {
        let s = "2026-05-14 17:49:11.400803100";
        let rfc = ch_datetime64_to_rfc3339(s);
        assert!(rfc.starts_with("2026-05-14T17:49:11"), "got: {rfc}");
    }

    #[test]
    fn ch_datetime64_returns_empty_for_empty_input() {
        assert_eq!(ch_datetime64_to_rfc3339(""), "");
    }

    #[tokio::test]
    async fn query_logs_returns_rows() {
        let body = r#"{"timestamp":1000000000,"severity_text":"INFO","service_name":"svc","body":"hello","trace_id":"abc","severity_number":9,"span_id":"sp1","attributes":"{\"k\":\"v\"}"}"#;
        let server = get_server(200, body).await;
        let rows = make_reader(server.uri())
            .query_logs(None, None, None, 100)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].service, "svc");
        assert_eq!(rows[0].body, "hello");
        assert_eq!(rows[0].severity, "INFO");
        assert_eq!(rows[0].severity_number, 9);
        assert_eq!(rows[0].span_id, "sp1");
        assert_eq!(rows[0].attributes["k"], "v");
    }

    #[tokio::test]
    async fn query_logs_histogram_returns_buckets() {
        let body = r#"{"bucket":"2024-01-01 00:00:00","count":5}"#;
        let server = get_server(200, body).await;
        let buckets = make_reader(server.uri())
            .query_logs_histogram(
                None,
                Some("2024-01-01T00:00:00Z"),
                Some("2024-01-01T01:00:00Z"),
                Some(60),
            )
            .await
            .unwrap();
        // Gap-filling generates one bucket per 60s step from from..=to: 61 total.
        // The bucket returned by ClickHouse (00:00:00, count=5) is merged in;
        // all others get count=0.
        assert_eq!(buckets.len(), 61);
        assert_eq!(buckets[0].bucket, "2024-01-01 00:00:00");
        assert_eq!(buckets[0].count, 5);
        assert_eq!(buckets[1].count, 0);
    }

    #[tokio::test]
    async fn query_logs_histogram_auto_step() {
        let body = "";
        let server = get_server(200, body).await;
        make_reader(server.uri())
            .query_logs_histogram(
                None,
                Some("2024-01-01T00:00:00Z"),
                Some("2024-01-01T01:00:00Z"),
                None,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn query_logs_facets_returns_fields() {
        let body = r#"{"value":"myapp","count":10}"#;
        let server = get_server(200, body).await;
        let facets = make_reader(server.uri())
            .query_logs_facets(None, None, None)
            .await
            .unwrap();
        assert_eq!(facets.len(), 2);
        assert_eq!(facets[0].field, "service_name");
        assert!(!facets[0].values.is_empty());
        assert_eq!(facets[0].values[0].value, "myapp");
        assert_eq!(facets[0].values[0].count, 10);
    }

    #[test]
    fn auto_step_targets_30_buckets() {
        assert_eq!(auto_step(30 * 60), 60);
        assert_eq!(auto_step(30 * 3600), 3600);
        assert_eq!(auto_step(30 * 86400), 86400);
    }

    #[test]
    fn build_where_clause_uses_todatetime64_string_literal() {
        let clause = build_where_clause(
            None,
            Some("2024-01-01T00:00:00Z"),
            Some("2024-12-31T23:59:59Z"),
            filter::to_clickhouse_logs,
            "timestamp",
        )
        .unwrap();
        assert!(
            clause.contains("toDateTime64('2024-01-01"),
            "expected string literal from clause: {clause}"
        );
        assert!(
            clause.contains("toDateTime64('2024-12-31"),
            "expected string literal to clause: {clause}"
        );
    }

    #[tokio::test]
    async fn query_logs_with_filter_and_range() {
        let server = get_server(200, "").await;
        make_reader(server.uri())
            .query_logs(
                Some("service:myapp"),
                Some("2024-01-01T00:00:00Z"),
                Some("2024-12-31T23:59:59Z"),
                50,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn query_logs_rejects_bad_filter() {
        let server = get_server(200, "").await;
        let err = make_reader(server.uri())
            .query_logs(Some("badtoken"), None, None, 100)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("filter parse error"));
    }

    #[tokio::test]
    async fn query_logs_rejects_bad_from() {
        let server = get_server(200, "").await;
        let err = make_reader(server.uri())
            .query_logs(None, Some("not-a-date"), None, 100)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid RFC 3339"));
    }

    #[tokio::test]
    async fn query_logs_propagates_clickhouse_error() {
        let server = get_server(500, "DB error").await;
        let err = make_reader(server.uri())
            .query_logs(None, None, None, 100)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("clickhouse query failed"));
    }

    #[tokio::test]
    async fn query_traces_groups_by_trace_id() {
        let body = concat!(
            r#"{"trace_id":"t1","span_id":"s1","parent_span_id":"","service_name":"svc","operation_name":"op","start_time_unix_nano":0,"end_time_unix_nano":1000000,"duration_ns":1000000,"status_code":0}"#,
            "\n",
            r#"{"trace_id":"t1","span_id":"s2","parent_span_id":"s1","service_name":"svc","operation_name":"child","start_time_unix_nano":0,"end_time_unix_nano":500000,"duration_ns":500000,"status_code":0}"#,
        );
        let server = get_server(200, body).await;
        let groups = make_reader(server.uri())
            .query_traces(None, None, None, 100)
            .await
            .unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].trace_id, "t1");
        assert_eq!(groups[0].spans.len(), 2);
    }

    #[tokio::test]
    async fn query_logs_sends_limit_in_sql() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(query_param_contains("query", "LIMIT 42"))
            .respond_with(ResponseTemplate::new(200).set_body_string(""))
            .mount(&server)
            .await;
        make_reader(server.uri())
            .query_logs(None, None, None, 42)
            .await
            .unwrap();
    }

    // Scenario: query_trace returns spans for a known trace ID
    #[tokio::test]
    async fn query_trace_returns_spans_for_known_id() {
        let body = concat!(
            r#"{"span_id":"s1","parent_span_id":"","service_name":"svc","operation_name":"op","start_time_unix_nano":0,"end_time_unix_nano":1000000,"duration_ns":1000000,"status_code":0}"#,
            "\n",
            r#"{"span_id":"s2","parent_span_id":"s1","service_name":"svc","operation_name":"child","start_time_unix_nano":100,"end_time_unix_nano":500000,"duration_ns":499900,"status_code":0}"#,
        );
        let server = get_server(200, body).await;
        let spans = make_reader(server.uri())
            .query_trace("abc123")
            .await
            .unwrap();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].span_id, "s1");
        assert_eq!(spans[0].service, "svc");
        assert_eq!(spans[1].span_id, "s2");
        assert_eq!(spans[1].parent_span_id, "s1");
    }

    // Scenario: query_trace returns empty Vec when ClickHouse returns no rows
    #[tokio::test]
    async fn query_trace_returns_empty_for_unknown_id() {
        let server = get_server(200, "").await;
        let spans = make_reader(server.uri())
            .query_trace("unknown-id")
            .await
            .unwrap();
        assert!(spans.is_empty());
    }

    // Scenario: query_trace sends trace_id in the SQL WHERE clause
    #[tokio::test]
    async fn query_trace_sends_trace_id_in_sql() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(query_param_contains("query", "trace_id = 'my-trace-id'"))
            .respond_with(ResponseTemplate::new(200).set_body_string(""))
            .mount(&server)
            .await;
        make_reader(server.uri())
            .query_trace("my-trace-id")
            .await
            .unwrap();
    }

    // Scenario: span with non-empty SpanAttributes map returns key-value pairs in `attributes`
    #[tokio::test]
    async fn query_trace_span_with_attributes_returns_key_value_pairs() {
        let body = r#"{"span_id":"s1","parent_span_id":"","service_name":"svc","operation_name":"op","start_time_unix_nano":0,"end_time_unix_nano":1000000,"duration_ns":1000000,"status_code":0,"attributes":"{\"http.method\":\"GET\",\"http.status_code\":\"200\"}"}"#;
        let server = get_server(200, body).await;
        let spans = make_reader(server.uri()).query_trace("t1").await.unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].attributes["http.method"], "GET");
        assert_eq!(spans[0].attributes["http.status_code"], "200");
    }

    // Scenario: span with no attributes returns an empty JSON object `{}`
    #[tokio::test]
    async fn query_trace_span_with_no_attributes_returns_empty_object() {
        let body = r#"{"span_id":"s1","parent_span_id":"","service_name":"svc","operation_name":"op","start_time_unix_nano":0,"end_time_unix_nano":1000000,"duration_ns":1000000,"status_code":0}"#;
        let server = get_server(200, body).await;
        let spans = make_reader(server.uri()).query_trace("t1").await.unwrap();
        assert_eq!(spans.len(), 1);
        assert!(
            spans[0].attributes.is_object(),
            "expected object, got: {}",
            spans[0].attributes
        );
        assert_eq!(spans[0].attributes.as_object().unwrap().len(), 0);
    }

    // Scenario: span with malformed attributes JSON falls back to empty object
    #[tokio::test]
    async fn query_trace_span_with_malformed_attributes_falls_back_to_empty_object() {
        let body = r#"{"span_id":"s1","parent_span_id":"","service_name":"svc","operation_name":"op","start_time_unix_nano":0,"end_time_unix_nano":1000000,"duration_ns":1000000,"status_code":0,"attributes":"not-valid-json"}"#;
        let server = get_server(200, body).await;
        let spans = make_reader(server.uri()).query_trace("t1").await.unwrap();
        assert_eq!(spans.len(), 1);
        assert!(spans[0].attributes.is_object());
        assert_eq!(spans[0].attributes.as_object().unwrap().len(), 0);
    }

    // Scenario: query_trace SQL includes `attributes` column
    #[tokio::test]
    async fn query_trace_sql_includes_attributes_column() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(query_param_contains("query", "attributes"))
            .respond_with(ResponseTemplate::new(200).set_body_string(""))
            .mount(&server)
            .await;
        make_reader(server.uri()).query_trace("t1").await.unwrap();
    }

    // Scenario: ChLogRow with string timestamp (ClickHouse DateTime64 format) converts correctly
    #[tokio::test]
    async fn query_logs_string_timestamp_converts_to_rfc3339() {
        let body = r#"{"timestamp":"2026-05-14 17:49:11.400803100","severity_text":"INFO","service_name":"svc","body":"hello","trace_id":"","severity_number":9,"span_id":"","attributes":{}}"#;
        let server = get_server(200, body).await;
        let rows = make_reader(server.uri())
            .query_logs(None, None, None, 100)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].timestamp.starts_with("2026-05-14T17:49:11"),
            "expected RFC3339, got: {}",
            rows[0].timestamp
        );
    }

    // Scenario: ChLogRow with null/unexpected timestamp type produces empty string
    #[tokio::test]
    async fn query_logs_null_timestamp_produces_empty_string() {
        let body = r#"{"timestamp":null,"severity_text":"INFO","service_name":"svc","body":"hi","trace_id":"","severity_number":0,"span_id":"","attributes":{}}"#;
        let server = get_server(200, body).await;
        let rows = make_reader(server.uri())
            .query_logs(None, None, None, 100)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].timestamp, "");
    }

    // Scenario: histogram From converts bucket/count fields correctly
    #[test]
    fn histogram_from_ch_row_maps_fields() {
        let row = super::ChHistogramRow {
            bucket: "2024-01-01 00:00:00".to_string(),
            count: 42,
        };
        let bucket = HistogramBucket::from(row);
        assert_eq!(bucket.bucket, "2024-01-01 00:00:00");
        assert_eq!(bucket.count, 42);
    }

    // Scenario: span with events and links populates those fields
    #[tokio::test]
    async fn query_trace_span_with_events_and_links() {
        let body = r#"{"span_id":"s1","parent_span_id":"","service_name":"svc","operation_name":"op","start_time_unix_nano":0,"end_time_unix_nano":1000000,"duration_ns":1000000,"status_code":0,"Events.Name":["evt"],"Events.Timestamp":[500000],"Events.Attributes":[{"k":"v"}],"Links.TraceId":["lt1"],"Links.SpanId":["ls1"],"Links.Attributes":[{}]}"#;
        let server = get_server(200, body).await;
        let spans = make_reader(server.uri()).query_trace("t1").await.unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].events.len(), 1);
        assert_eq!(spans[0].events[0].name, "evt");
        assert_eq!(spans[0].links.len(), 1);
        assert_eq!(spans[0].links[0].trace_id, "lt1");
        assert_eq!(spans[0].links[0].span_id, "ls1");
    }

    // Scenario: facets query error on one field still returns empty values (not a hard error)
    #[tokio::test]
    async fn query_logs_facets_ch_error_returns_empty_values() {
        let server = get_server(500, "DB error").await;
        let facets = make_reader(server.uri())
            .query_logs_facets(None, None, None)
            .await
            .unwrap();
        assert_eq!(facets.len(), 2);
        assert!(facets[0].values.is_empty());
        assert!(facets[1].values.is_empty());
    }

    #[tokio::test]
    async fn query_traces_facets_returns_fields() {
        let body = r#"{"value":"payment-service","count":5}"#;
        let server = get_server(200, body).await;
        let facets = make_reader(server.uri())
            .query_traces_facets(None, None, None)
            .await
            .unwrap();
        assert_eq!(facets.len(), 2);
        assert_eq!(facets[0].field, "service_name");
        assert!(!facets[0].values.is_empty());
        assert_eq!(facets[0].values[0].value, "payment-service");
        assert_eq!(facets[0].values[0].count, 5);
    }

    #[tokio::test]
    async fn query_traces_facets_ch_error_returns_empty_values() {
        let server = get_server(500, "DB error").await;
        let facets = make_reader(server.uri())
            .query_traces_facets(None, None, None)
            .await
            .unwrap();
        assert_eq!(facets.len(), 2);
        assert!(facets[0].values.is_empty());
        assert!(facets[1].values.is_empty());
    }
}
