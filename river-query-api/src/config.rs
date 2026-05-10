use anyhow::Result;
use river_config::StorageConfig;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub api_port: u16,
    #[serde(flatten)]
    pub storage: StorageConfig,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        river_config::build(
            river_config::storage_defaults(river_config::builder())?
                .set_default("api_port", 8080)?,
        )
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn reads_port_from_env() {
        temp_env::with_vars(
            [
                ("RIVER_CLICKHOUSE_USER", Some("u")),
                ("RIVER_CLICKHOUSE_PASSWORD", Some("p")),
                ("RIVER_API_PORT", Some("9090")),
            ],
            || {
                let cfg = Config::from_env().expect("should load");
                assert_eq!(cfg.api_port, 9090);
            },
        );
    }

    #[test]
    fn defaults_api_port_to_8080() {
        temp_env::with_vars(
            [
                ("RIVER_CLICKHOUSE_USER", Some("u")),
                ("RIVER_CLICKHOUSE_PASSWORD", Some("p")),
                ("RIVER_API_PORT", None::<&str>),
            ],
            || {
                let cfg = Config::from_env().expect("should load");
                assert_eq!(cfg.api_port, 8080);
            },
        );
    }
}
