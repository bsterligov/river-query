mod clickhouse;
mod config;
mod filter;
mod victoriametrics;

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::{OpenApi, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_swagger_ui::SwaggerUi;

use clickhouse::Reader as ChReader;
use victoriametrics::Reader as VmReader;

struct AppState {
    ch: ChReader,
    vm: VmReader,
}

#[derive(Debug, Serialize, ToSchema)]
struct ErrorBody {
    error: String,
}

enum ApiError {
    BadRequest(String),
    ServiceUnavailable(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::BadRequest(msg) => {
                (StatusCode::BAD_REQUEST, Json(ErrorBody { error: msg })).into_response()
            }
            ApiError::ServiceUnavailable(msg) => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorBody { error: msg }),
            )
                .into_response(),
        }
    }
}

fn parse_step(s: &str) -> Option<u64> {
    if let Some(n) = s.strip_suffix('s') {
        n.parse().ok()
    } else if let Some(n) = s.strip_suffix('m') {
        n.parse::<u64>().ok().map(|v| v * 60)
    } else if let Some(n) = s.strip_suffix('h') {
        n.parse::<u64>().ok().map(|v| v * 3600)
    } else {
        s.parse().ok()
    }
}

fn map_backend_error(err: anyhow::Error) -> ApiError {
    let msg = err.to_string();
    if msg.contains("filter") || msg.contains("invalid RFC 3339") {
        ApiError::BadRequest(msg)
    } else {
        ApiError::ServiceUnavailable(msg)
    }
}

#[derive(Debug, Deserialize, ToSchema)]
struct RangeParams {
    filter: Option<String>,
    from: Option<String>,
    to: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, ToSchema)]
struct MetricsParams {
    filter: Option<String>,
    from: Option<String>,
    to: Option<String>,
    step: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
struct HistogramParams {
    filter: Option<String>,
    from: Option<String>,
    to: Option<String>,
    step: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
struct FacetsParams {
    filter: Option<String>,
    from: Option<String>,
    to: Option<String>,
}

#[utoipa::path(
    get,
    path = "/v1/logs",
    params(
        ("filter" = Option<String>, Query, description = "Filter expression"),
        ("from" = Option<String>, Query, description = "Start time (RFC 3339)"),
        ("to" = Option<String>, Query, description = "End time (RFC 3339)"),
        ("limit" = Option<u32>, Query, description = "Max rows (default 100, max 1000)"),
    ),
    responses(
        (status = 200, description = "Log entries", body = Vec<clickhouse::LogRow>),
        (status = 400, description = "Invalid filter or timestamp", body = ErrorBody),
        (status = 503, description = "ClickHouse unavailable", body = ErrorBody),
    )
)]
async fn get_logs(
    State(state): State<Arc<AppState>>,
    Query(params): Query<RangeParams>,
) -> Result<Json<Vec<clickhouse::LogRow>>, ApiError> {
    let limit = params.limit.unwrap_or(100).min(1000);
    state
        .ch
        .query_logs(
            params.filter.as_deref(),
            params.from.as_deref(),
            params.to.as_deref(),
            limit,
        )
        .await
        .map(Json)
        .map_err(map_backend_error)
}

#[utoipa::path(
    get,
    path = "/v1/traces",
    params(
        ("filter" = Option<String>, Query, description = "Filter expression"),
        ("from" = Option<String>, Query, description = "Start time (RFC 3339)"),
        ("to" = Option<String>, Query, description = "End time (RFC 3339)"),
        ("limit" = Option<u32>, Query, description = "Max spans (default 100, max 1000)"),
    ),
    responses(
        (status = 200, description = "Traces grouped by trace_id", body = Vec<clickhouse::TraceGroup>),
        (status = 400, description = "Invalid filter or timestamp", body = ErrorBody),
        (status = 503, description = "ClickHouse unavailable", body = ErrorBody),
    )
)]
async fn get_traces(
    State(state): State<Arc<AppState>>,
    Query(params): Query<RangeParams>,
) -> Result<Json<Vec<clickhouse::TraceGroup>>, ApiError> {
    let limit = params.limit.unwrap_or(100).min(1000);
    state
        .ch
        .query_traces(
            params.filter.as_deref(),
            params.from.as_deref(),
            params.to.as_deref(),
            limit,
        )
        .await
        .map(Json)
        .map_err(map_backend_error)
}

#[utoipa::path(
    get,
    path = "/v1/metrics",
    params(
        ("filter" = Option<String>, Query, description = "Filter expression"),
        ("from" = Option<String>, Query, description = "Start time (RFC 3339)"),
        ("to" = Option<String>, Query, description = "End time (RFC 3339)"),
        ("step" = Option<String>, Query, description = "Query step (e.g. 60s, 5m)"),
    ),
    responses(
        (status = 200, description = "Metric time series points", body = Vec<victoriametrics::MetricPoint>),
        (status = 400, description = "Invalid filter or timestamp", body = ErrorBody),
        (status = 503, description = "VictoriaMetrics unavailable", body = ErrorBody),
    )
)]
async fn get_metrics(
    State(state): State<Arc<AppState>>,
    Query(params): Query<MetricsParams>,
) -> Result<Json<Vec<victoriametrics::MetricPoint>>, ApiError> {
    let filter = params.filter.as_deref().unwrap_or("");
    let from = params.from.as_deref().unwrap_or("");
    let to = params.to.as_deref().unwrap_or("");
    let step = params.step.as_deref().unwrap_or("60s");

    if from.is_empty() || to.is_empty() {
        return Err(ApiError::BadRequest(
            "'from' and 'to' are required for metrics queries".to_string(),
        ));
    }

    state
        .vm
        .query_metrics(filter, from, to, step)
        .await
        .map(Json)
        .map_err(map_backend_error)
}

#[utoipa::path(
    get,
    path = "/v1/logs/histogram",
    params(
        ("filter" = Option<String>, Query, description = "Filter expression"),
        ("from" = Option<String>, Query, description = "Start time (RFC 3339)"),
        ("to" = Option<String>, Query, description = "End time (RFC 3339)"),
        ("step" = Option<String>, Query, description = "Bucket width (e.g. 60s, 5m); auto-selected if omitted"),
    ),
    responses(
        (status = 200, description = "Log counts per time bucket", body = Vec<clickhouse::HistogramBucket>),
        (status = 400, description = "Invalid filter, timestamp, or step", body = ErrorBody),
        (status = 503, description = "ClickHouse unavailable", body = ErrorBody),
    )
)]
async fn get_logs_histogram(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HistogramParams>,
) -> Result<Json<Vec<clickhouse::HistogramBucket>>, ApiError> {
    let step_secs = params
        .step
        .as_deref()
        .map(|s| {
            parse_step(s).ok_or_else(|| ApiError::BadRequest(format!("invalid step value: '{s}'")))
        })
        .transpose()?;

    state
        .ch
        .query_logs_histogram(
            params.filter.as_deref(),
            params.from.as_deref(),
            params.to.as_deref(),
            step_secs,
        )
        .await
        .map(Json)
        .map_err(map_backend_error)
}

#[utoipa::path(
    get,
    path = "/v1/logs/facets",
    params(
        ("filter" = Option<String>, Query, description = "Filter expression"),
        ("from" = Option<String>, Query, description = "Start time (RFC 3339)"),
        ("to" = Option<String>, Query, description = "End time (RFC 3339)"),
    ),
    responses(
        (status = 200, description = "Top values per facet field", body = Vec<clickhouse::FacetField>),
        (status = 400, description = "Invalid filter or timestamp", body = ErrorBody),
        (status = 503, description = "ClickHouse unavailable", body = ErrorBody),
    )
)]
async fn get_logs_facets(
    State(state): State<Arc<AppState>>,
    Query(params): Query<FacetsParams>,
) -> Result<Json<Vec<clickhouse::FacetField>>, ApiError> {
    state
        .ch
        .query_logs_facets(
            params.filter.as_deref(),
            params.from.as_deref(),
            params.to.as_deref(),
        )
        .await
        .map(Json)
        .map_err(map_backend_error)
}

#[utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, description = "Service is healthy"),
    )
)]
async fn get_health() -> StatusCode {
    StatusCode::OK
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_logs,
        get_logs_histogram,
        get_logs_facets,
        get_traces,
        get_metrics,
        get_health
    ),
    components(schemas(
        clickhouse::LogRow,
        clickhouse::HistogramBucket,
        clickhouse::FacetValue,
        clickhouse::FacetField,
        clickhouse::Span,
        clickhouse::TraceGroup,
        victoriametrics::MetricPoint,
        ErrorBody,
    ))
)]
struct ApiDoc;

fn build_router(state: Arc<AppState>) -> axum::Router {
    let (api_router, _) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(utoipa_axum::routes!(get_logs))
        .routes(utoipa_axum::routes!(get_logs_histogram))
        .routes(utoipa_axum::routes!(get_logs_facets))
        .routes(utoipa_axum::routes!(get_traces))
        .routes(utoipa_axum::routes!(get_metrics))
        .split_for_parts();
    axum::Router::new()
        .merge(api_router)
        .route("/health", get(get_health))
        .merge(SwaggerUi::new("/swagger-ui").url("/openapi.json", ApiDoc::openapi()))
        .with_state(state)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = config::Config::from_env()?;
    let http = reqwest::Client::new();

    let state = Arc::new(AppState {
        ch: ChReader::new(
            http.clone(),
            cfg.storage.clickhouse_url,
            cfg.storage.clickhouse_db,
            cfg.storage.clickhouse_user,
            cfg.storage.clickhouse_password,
        ),
        vm: VmReader::new(http, cfg.storage.victoriametrics_url),
    });

    let app = build_router(state);

    let addr = format!("0.0.0.0:{}", cfg.api_port);
    println!("river api listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::util::ServiceExt;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_state(ch_url: String, vm_url: String) -> Arc<AppState> {
        let http = reqwest::Client::new();
        Arc::new(AppState {
            ch: ChReader::new(
                http.clone(),
                ch_url,
                "river".to_string(),
                "river".to_string(),
                "river".to_string(),
            ),
            vm: VmReader::new(http, vm_url),
        })
    }

    fn build_app(ch_url: String, vm_url: String) -> axum::Router {
        build_router(make_state(ch_url, vm_url))
    }

    async fn mock_server(status: u16, body: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(&server)
            .await;
        server
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    // Scenario: GET /health returns 200
    #[tokio::test]
    async fn health_returns_200() {
        let app = build_app(
            "http://127.0.0.1:1".to_string(),
            "http://127.0.0.1:1".to_string(),
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    // Scenario: GET /openapi.json returns a JSON spec
    #[tokio::test]
    async fn openapi_json_is_valid() {
        let app = build_app(
            "http://127.0.0.1:1".to_string(),
            "http://127.0.0.1:1".to_string(),
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let val = body_json(resp).await;
        assert!(val.get("openapi").is_some());
        assert!(val.get("paths").is_some());
    }

    // Scenario: GET /swagger-ui/ serves the Swagger UI HTML
    #[tokio::test]
    async fn swagger_ui_is_accessible() {
        let app = build_app(
            "http://127.0.0.1:1".to_string(),
            "http://127.0.0.1:1".to_string(),
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/swagger-ui/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    // Scenario: query logs with filter — returns matching log entries
    #[tokio::test]
    async fn get_logs_with_filter_returns_200() {
        let body = r#"{"timestamp":1000000000,"severity_text":"ERROR","service_name":"myapp","body":"err","trace_id":""}"#;
        let ch = mock_server(200, body).await;
        let vm = mock_server(200, "").await;
        let app = build_app(ch.uri(), vm.uri());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/logs?filter=service%3Amyapp+AND+level%3Aerror")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let rows = body_json(resp).await;
        assert!(rows.is_array());
    }

    // Scenario: invalid filter syntax — returns 400 with human-readable error
    #[tokio::test]
    async fn get_logs_invalid_filter_returns_400() {
        let ch = mock_server(200, "").await;
        let vm = mock_server(200, "").await;
        let app = build_app(ch.uri(), vm.uri());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/logs?filter=notafilter")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
        let body = body_json(resp).await;
        assert!(body["error"].as_str().is_some());
    }

    // Scenario: unhealthy backend — ClickHouse returns 503
    #[tokio::test]
    async fn get_logs_clickhouse_down_returns_503() {
        let ch = mock_server(500, "connection refused").await;
        let vm = mock_server(200, "").await;
        let app = build_app(ch.uri(), vm.uri());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/logs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 503);
        let body = body_json(resp).await;
        assert!(body["error"].as_str().is_some());
    }

    // Scenario: query traces with filter — returns grouped by trace_id
    #[tokio::test]
    async fn get_traces_with_filter_returns_200() {
        let body = r#"{"trace_id":"t1","span_id":"s1","parent_span_id":"","service_name":"myapp","operation_name":"op","start_time_unix_nano":0,"end_time_unix_nano":600000000,"duration_ns":600000000,"status_code":0}"#;
        let ch = mock_server(200, body).await;
        let vm = mock_server(200, "").await;
        let app = build_app(ch.uri(), vm.uri());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/traces?filter=service%3Amyapp+AND+duration_ms%3A%3E500")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let groups = body_json(resp).await;
        assert!(groups.is_array());
        assert_eq!(groups[0]["trace_id"], "t1");
    }

    // Scenario: query metrics — returns time series points
    #[tokio::test]
    async fn get_metrics_returns_points() {
        let body = r#"{"status":"success","data":{"resultType":"matrix","result":[{"metric":{},"values":[[1704067200,"42"],[1704067260,"43"]]}]}}"#;
        let ch = mock_server(200, "").await;
        let vm = mock_server(200, body).await;
        let app = build_app(ch.uri(), vm.uri());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/metrics?filter=name%3Ahttp_requests_total+AND+service%3Amyapp&from=2024-01-01T00%3A00%3A00Z&to=2024-01-01T01%3A00%3A00Z&step=60s")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let points = body_json(resp).await;
        assert!(points.is_array());
        assert_eq!(points.as_array().unwrap().len(), 2);
    }

    // Scenario: unhealthy VictoriaMetrics — returns 503
    #[tokio::test]
    async fn get_metrics_vm_down_returns_503() {
        let ch = mock_server(200, "").await;
        let vm = mock_server(503, "unavailable").await;
        let app = build_app(ch.uri(), vm.uri());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/metrics?filter=name%3Afoo&from=2024-01-01T00%3A00%3A00Z&to=2024-01-01T01%3A00%3A00Z")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 503);
    }

    // Scenario: metrics missing from/to — returns 400
    #[tokio::test]
    async fn get_metrics_missing_from_to_returns_400() {
        let ch = mock_server(200, "").await;
        let vm = mock_server(200, "").await;
        let app = build_app(ch.uri(), vm.uri());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/metrics?filter=name%3Afoo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
    }

    // Scenario: GET /v1/logs/histogram returns buckets
    #[tokio::test]
    async fn get_logs_histogram_returns_200() {
        let body = r#"{"bucket":"2024-01-01 00:00:00","count":5}"#;
        let ch = mock_server(200, body).await;
        let vm = mock_server(200, "").await;
        let app = build_app(ch.uri(), vm.uri());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/logs/histogram?from=2024-01-01T00%3A00%3A00Z&to=2024-01-01T01%3A00%3A00Z")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let val = body_json(resp).await;
        assert!(val.is_array());
        assert_eq!(val[0]["bucket"], "2024-01-01 00:00:00");
        assert_eq!(val[0]["count"], 5);
    }

    // Scenario: GET /v1/logs/facets returns facet fields
    #[tokio::test]
    async fn get_logs_facets_returns_200() {
        let body = r#"{"value":"myapp","count":10}"#;
        let ch = mock_server(200, body).await;
        let vm = mock_server(200, "").await;
        let app = build_app(ch.uri(), vm.uri());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/logs/facets")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let val = body_json(resp).await;
        assert!(val.is_array());
        assert!(val[0]["field"].as_str().is_some());
        assert!(val[0]["values"].is_array());
    }

    // Scenario: limit is capped at 1000
    #[tokio::test]
    async fn get_logs_limit_capped_at_1000() {
        let ch = mock_server(200, "").await;
        let vm = mock_server(200, "").await;
        let app = build_app(ch.uri(), vm.uri());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/logs?limit=9999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }
}
