use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::filter;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct MetricPoint {
    pub timestamp: String,
    pub value: f64,
}

pub struct Reader {
    client: reqwest::Client,
    base_url: String,
}

impl Reader {
    pub fn new(client: reqwest::Client, base_url: String) -> Self {
        Reader { client, base_url }
    }

    pub async fn list_metric_names(&self) -> Result<Vec<String>> {
        let url = format!("{}/api/v1/label/__name__/values", self.base_url);
        let resp = self.client.get(&url).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("victoriametrics label values failed status={status}: {body}");
        }

        let body: serde_json::Value = resp.json().await?;
        let names = body["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        Ok(names)
    }

    pub async fn query_metrics(
        &self,
        filter: &str,
        from: &str,
        to: &str,
        step: &str,
    ) -> Result<Vec<MetricPoint>> {
        let query = build_vm_query(filter)?;

        let url = format!("{}/api/v1/query_range", self.base_url);
        let resp = self
            .client
            .get(&url)
            .query(&[
                ("query", query.as_str()),
                ("start", from),
                ("end", to),
                ("step", step),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("victoriametrics query failed status={status}: {body}");
        }

        let body: serde_json::Value = resp.json().await?;
        Ok(parse_vm_response(&body))
    }
}

fn parse_vm_response(body: &serde_json::Value) -> Vec<MetricPoint> {
    body["data"]["result"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(parse_vm_series)
        .collect()
}

fn parse_vm_series(series: &serde_json::Value) -> Vec<MetricPoint> {
    series["values"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_vm_point)
        .collect()
}

fn parse_vm_point(pair: &serde_json::Value) -> Option<MetricPoint> {
    let ts = pair[0].as_f64()?;
    let value: f64 = pair[1].as_str()?.parse().unwrap_or(0.0);
    Some(MetricPoint {
        timestamp: unix_secs_to_rfc3339(ts as i64),
        value,
    })
}

fn build_vm_query(filter: &str) -> Result<String> {
    let expr = filter::parse(filter).map_err(|e| anyhow::anyhow!("filter parse error: {e}"))?;

    match expr {
        None => Ok("{}".to_string()),
        Some(expr) => {
            let selector = filter::to_vm_selector(&expr)
                .map_err(|e| anyhow::anyhow!("filter translate error: {e}"))?;
            Ok(selector)
        }
    }
}

fn unix_secs_to_rfc3339(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_reader(url: String) -> Reader {
        Reader::new(reqwest::Client::new(), url)
    }

    fn vm_response(values: &[(&str, &str)]) -> String {
        let value_arr: Vec<String> = values
            .iter()
            .map(|(ts, v)| format!("[{ts}, \"{v}\"]"))
            .collect();
        format!(
            r#"{{"status":"success","data":{{"resultType":"matrix","result":[{{"metric":{{}},"values":[{}]}}]}}}}"#,
            value_arr.join(",")
        )
    }

    #[tokio::test]
    async fn query_metrics_returns_points() {
        let body = vm_response(&[("1704067200", "42"), ("1704067260", "43")]);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let points = make_reader(server.uri())
            .query_metrics(
                "name:http_requests_total AND service:myapp",
                "2024-01-01T00:00:00Z",
                "2024-01-01T01:00:00Z",
                "60s",
            )
            .await
            .unwrap();
        assert_eq!(points.len(), 2);
        assert_eq!(points[0].value, 42.0);
        assert_eq!(points[1].value, 43.0);
    }

    #[tokio::test]
    async fn query_metrics_propagates_vm_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503).set_body_string("unavailable"))
            .mount(&server)
            .await;
        let err = make_reader(server.uri())
            .query_metrics(
                "name:foo",
                "2024-01-01T00:00:00Z",
                "2024-01-02T00:00:00Z",
                "60s",
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("victoriametrics query failed"));
    }

    #[tokio::test]
    async fn query_metrics_rejects_bad_filter() {
        let server = MockServer::start().await;
        let err = make_reader(server.uri())
            .query_metrics(
                "badtoken",
                "2024-01-01T00:00:00Z",
                "2024-01-02T00:00:00Z",
                "60s",
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("filter parse error"));
    }

    #[test]
    fn build_vm_query_single_label() {
        let q = build_vm_query("name:http_requests_total").unwrap();
        assert_eq!(q, "{__name__=\"http_requests_total\"}");
    }

    #[test]
    fn build_vm_query_multiple_labels() {
        let q = build_vm_query("name:http_requests_total AND service:myapp").unwrap();
        assert!(q.contains("__name__=\"http_requests_total\""));
        assert!(q.contains("service=\"myapp\""));
    }

    #[test]
    fn build_vm_query_empty_filter() {
        let q = build_vm_query("").unwrap();
        assert_eq!(q, "{}");
    }
}
