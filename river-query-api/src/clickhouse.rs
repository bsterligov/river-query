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
            "SELECT timestamp, severity_text, service_name, body, trace_id \
             FROM logs{where_clause} \
             ORDER BY timestamp DESC \
             LIMIT {limit} \
             FORMAT JSONEachRow"
        );

        let rows = self.query_json(&sql).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(LogRow {
                timestamp: ns_to_rfc3339(row["timestamp"].as_u64().unwrap_or(0)),
                severity: row["severity_text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                service: row["service_name"].as_str().unwrap_or_default().to_string(),
                body: row["body"].as_str().unwrap_or_default().to_string(),
                trace_id: row["trace_id"].as_str().unwrap_or_default().to_string(),
            });
        }
        Ok(out)
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
        clauses.push(format!("{time_field} >= {}", rfc3339_to_ns(ts)?));
    }
    if let Some(ts) = to {
        clauses.push(format!("{time_field} <= {}", rfc3339_to_ns(ts)?));
    }

    if clauses.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!(" WHERE {}", clauses.join(" AND ")))
    }
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

    #[tokio::test]
    async fn query_logs_returns_rows() {
        let body = r#"{"timestamp":1000000000,"severity_text":"INFO","service_name":"svc","body":"hello","trace_id":"abc"}"#;
        let server = get_server(200, body).await;
        let rows = make_reader(server.uri())
            .query_logs(None, None, None, 100)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].service, "svc");
        assert_eq!(rows[0].body, "hello");
        assert_eq!(rows[0].severity, "INFO");
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
