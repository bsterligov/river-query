use anyhow::{bail, Result};
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::filter;

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
    pub events: Vec<SpanEvent>,
    pub links: Vec<SpanLink>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TraceGroup {
    pub trace_id: String,
    pub spans: Vec<Span>,
}

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

        let rows = self.query_json(&sql).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let attributes = parse_attributes(&row["attributes"]);
            out.push(LogRow {
                timestamp: ch_datetime64_to_rfc3339(row["timestamp"].as_str().unwrap_or_default()),
                severity: row["severity_text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                service: row["service_name"].as_str().unwrap_or_default().to_string(),
                body: row["body"].as_str().unwrap_or_default().to_string(),
                trace_id: row["trace_id"].as_str().unwrap_or_default().to_string(),
                severity_number: row["severity_number"].as_i64().unwrap_or(0),
                span_id: row["span_id"].as_str().unwrap_or_default().to_string(),
                attributes,
            });
        }
        Ok(out)
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

        let rows = self.query_json(&sql).await?;

        // Build a map from bucket label → count for gap-filling below.
        let mut counts: std::collections::HashMap<String, u64> =
            std::collections::HashMap::with_capacity(rows.len());
        for row in &rows {
            let key = row["bucket"].as_str().unwrap_or_default().to_string();
            let count = row["count"].as_u64().unwrap_or(0);
            counts.insert(key, count);
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

    pub async fn query_logs_facets(
        &self,
        filter: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<FacetField>> {
        let where_clause =
            build_where_clause(filter, from, to, filter::to_clickhouse_logs, "timestamp")?;

        let fields = ["service_name", "severity_text"];
        let mut result = Vec::new();

        for field in fields {
            let sql = format!(
                "SELECT {field} AS value, count() AS count \
                 FROM logs{where_clause} \
                 GROUP BY {field} \
                 ORDER BY count DESC \
                 LIMIT 20 \
                 FORMAT JSONEachRow"
            );
            match self.query_json(&sql).await {
                Ok(rows) => {
                    let values = rows
                        .into_iter()
                        .map(|row| FacetValue {
                            value: row["value"].as_str().unwrap_or_default().to_string(),
                            count: row["count"].as_u64().unwrap_or(0),
                        })
                        .collect();
                    result.push(FacetField {
                        field: field.to_string(),
                        values,
                    });
                }
                Err(_) => {
                    result.push(FacetField {
                        field: field.to_string(),
                        values: vec![],
                    });
                }
            }
        }

        Ok(result)
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
             duration_ns, status_code, \
             `Events.Name`, \
             arrayMap(t -> toUnixTimestamp64Nano(t), `Events.Timestamp`) AS `Events.Timestamp`, \
             `Events.Attributes`, \
             `Links.TraceId`, `Links.SpanId`, `Links.Attributes` \
             FROM traces{where_clause} \
             ORDER BY start_time_unix_nano DESC \
             LIMIT {limit} \
             FORMAT JSONEachRow"
        );

        let rows = self.query_json(&sql).await?;
        let mut groups: std::collections::HashMap<String, Vec<Span>> =
            std::collections::HashMap::new();

        for row in rows {
            let trace_id = row["trace_id"].as_str().unwrap_or_default().to_string();
            let duration_ns = row["duration_ns"].as_u64().unwrap_or(0);

            let event_names = row["Events.Name"].as_array().cloned().unwrap_or_default();
            let event_timestamps = row["Events.Timestamp"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let event_attributes = row["Events.Attributes"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let events = event_names
                .into_iter()
                .zip(event_timestamps)
                .zip(event_attributes)
                .map(|((name, ts), attrs)| SpanEvent {
                    name: name.as_str().unwrap_or_default().to_string(),
                    timestamp: ns_to_rfc3339(ts.as_u64().unwrap_or(0)),
                    attributes: attrs,
                })
                .collect();

            let link_trace_ids = row["Links.TraceId"].as_array().cloned().unwrap_or_default();
            let link_span_ids = row["Links.SpanId"].as_array().cloned().unwrap_or_default();
            let link_attributes = row["Links.Attributes"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let links = link_trace_ids
                .into_iter()
                .zip(link_span_ids)
                .zip(link_attributes)
                .map(|((tid, sid), attrs)| SpanLink {
                    trace_id: tid.as_str().unwrap_or_default().to_string(),
                    span_id: sid.as_str().unwrap_or_default().to_string(),
                    attributes: attrs,
                })
                .collect();

            let span = Span {
                span_id: row["span_id"].as_str().unwrap_or_default().to_string(),
                parent_span_id: row["parent_span_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                service: row["service_name"].as_str().unwrap_or_default().to_string(),
                operation: row["operation_name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                start_time: ns_to_rfc3339(row["start_time_unix_nano"].as_u64().unwrap_or(0)),
                end_time: ns_to_rfc3339(row["end_time_unix_nano"].as_u64().unwrap_or(0)),
                duration_ms: duration_ns as f64 / 1_000_000.0,
                status_code: row["status_code"].as_i64().unwrap_or(0),
                events,
                links,
            };
            groups.entry(trace_id).or_default().push(span);
        }

        let mut result: Vec<TraceGroup> = groups
            .into_iter()
            .map(|(trace_id, spans)| TraceGroup { trace_id, spans })
            .collect();
        result.sort_by(|a, b| a.trace_id.cmp(&b.trace_id));
        Ok(result)
    }

    async fn query_json(&self, sql: &str) -> Result<Vec<serde_json::Value>> {
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
            let body = resp.text().await.unwrap_or_default();
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

fn auto_step(range_secs: u64) -> u64 {
    const LADDER: &[u64] = &[60, 300, 900, 3600, 21600, 86400];
    let target = range_secs / 30;
    *LADDER.iter().find(|&&s| s >= target).unwrap_or(&86400)
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
        .unwrap_or_default()
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
}
