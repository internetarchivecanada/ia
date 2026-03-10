use serde::Deserialize;

use crate::client::IaClient;
use crate::error::{IaError, Result};

/// Query parameters for the Tasks API.
#[derive(Debug, Default, Clone)]
pub struct TasksQuery {
    pub identifier: Option<String>,
    pub cmd: Option<String>,
    pub limit: Option<u32>,
    pub submitter: Option<String>,
    pub args: Option<String>,
}

/// Response envelope from the Tasks API.
#[derive(Debug, Clone, Deserialize)]
pub struct TasksResponse {
    pub success: bool,
    pub value: TasksValue,
}

/// The `value` field of the Tasks API response.
#[derive(Debug, Clone, Deserialize)]
pub struct TasksValue {
    pub summary: TasksSummary,
    #[serde(default)]
    pub catalog: Vec<TaskEntry>,
}

/// Aggregate task counts.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct TasksSummary {
    #[serde(default)]
    pub queued: u32,
    #[serde(default)]
    pub running: u32,
    #[serde(default)]
    pub error: u32,
    #[serde(default)]
    pub paused: u32,
}

/// A single task entry from the catalog.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskEntry {
    pub task_id: u64,
    pub identifier: String,
    pub cmd: String,
    #[serde(default)]
    pub submitter: String,
    #[serde(default)]
    pub submittime: String,
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub priority: i32,
}

/// Fetch tasks from the archive.org Tasks API (read-only).
pub async fn get_tasks(client: &IaClient, query: &TasksQuery) -> Result<TasksValue> {
    let (access, secret) = client.require_auth()?;
    let url = client.url("/services/tasks.php");

    let mut req = client
        .http()
        .get(&url)
        .header("Authorization", format!("LOW {access}:{secret}"));

    if let Some(ref id) = query.identifier {
        req = req.query(&[("identifier", id.as_str())]);
    }
    if let Some(ref cmd) = query.cmd {
        req = req.query(&[("cmd", cmd.as_str())]);
    }
    if let Some(limit) = query.limit {
        req = req.query(&[("limit", &limit.to_string())]);
    }
    if let Some(ref submitter) = query.submitter {
        req = req.query(&[("submitter", submitter.as_str())]);
    }
    if let Some(ref args) = query.args {
        req = req.query(&[("args", args.as_str())]);
    }

    let resp = req.send().await?;

    let status = resp.status();
    if !status.is_success() {
        return Err(IaError::Http {
            status: status.as_u16(),
            message: resp.text().await.unwrap_or_default(),
        });
    }

    let body = resp.text().await.map_err(reqwest_middleware::Error::from)?;
    let parsed: TasksResponse = serde_json::from_str(&body)?;
    if !parsed.success {
        return Err(IaError::Http {
            status: status.as_u16(),
            message: "Tasks API returned success: false".into(),
        });
    }
    Ok(parsed.value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn mock_config(server_uri: &str) -> crate::config::IaConfig {
        let mut config = crate::config::IaConfig::default();
        let host = server_uri
            .strip_prefix("http://")
            .or_else(|| server_uri.strip_prefix("https://"))
            .unwrap_or(server_uri);
        config.general.host = host.to_string();
        config.general.secure = false;
        config.s3_access = Some("test_access".to_string());
        config.s3_secret = Some("test_secret".to_string());
        config
    }

    #[tokio::test]
    async fn test_get_tasks_by_identifier() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("identifier", "my-test-item"))
            .and(header("Authorization", "LOW test_access:test_secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": {
                    "summary": {
                        "queued": 3,
                        "running": 1,
                        "error": 0,
                        "paused": 0
                    },
                    "catalog": [
                        {
                            "task_id": 123456789,
                            "identifier": "my-test-item",
                            "cmd": "derive.php",
                            "submitter": "user@example.com",
                            "submittime": "2026-03-09 12:00:00",
                            "server": "ia600100.us.archive.org",
                            "color": "green",
                            "priority": 10
                        }
                    ]
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery {
            identifier: Some("my-test-item".to_string()),
            ..Default::default()
        };

        let result = get_tasks(&client, &query).await.unwrap();

        // Verify summary
        assert_eq!(result.summary.queued, 3);
        assert_eq!(result.summary.running, 1);
        assert_eq!(result.summary.error, 0);
        assert_eq!(result.summary.paused, 0);

        // Verify catalog entry
        assert_eq!(result.catalog.len(), 1);
        let entry = &result.catalog[0];
        assert_eq!(entry.task_id, 123456789);
        assert_eq!(entry.identifier, "my-test-item");
        assert_eq!(entry.cmd, "derive.php");
        assert_eq!(entry.submitter, "user@example.com");
        assert_eq!(entry.submittime, "2026-03-09 12:00:00");
        assert_eq!(entry.server, "ia600100.us.archive.org");
        assert_eq!(entry.color, "green");
        assert_eq!(entry.priority, 10);
    }

    #[tokio::test]
    async fn test_get_tasks_empty_catalog() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(header("Authorization", "LOW test_access:test_secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": {
                    "summary": {
                        "queued": 0,
                        "running": 0,
                        "error": 0,
                        "paused": 0
                    }
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery::default();

        let result = get_tasks(&client, &query).await.unwrap();

        assert_eq!(result.summary.queued, 0);
        assert_eq!(result.summary.running, 0);
        assert_eq!(result.summary.error, 0);
        assert_eq!(result.summary.paused, 0);
        assert!(result.catalog.is_empty());
    }

    #[tokio::test]
    async fn test_get_tasks_with_cmd_and_limit() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("cmd", "derive.php"))
            .and(query_param("limit", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": {
                    "summary": {
                        "queued": 10,
                        "running": 2
                    },
                    "catalog": []
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery {
            cmd: Some("derive.php".to_string()),
            limit: Some(5),
            ..Default::default()
        };

        let result = get_tasks(&client, &query).await.unwrap();

        // Summary fields with serde(default) should handle missing fields
        assert_eq!(result.summary.queued, 10);
        assert_eq!(result.summary.running, 2);
        assert_eq!(result.summary.error, 0);
        assert_eq!(result.summary.paused, 0);
        assert!(result.catalog.is_empty());
    }

    #[tokio::test]
    async fn test_get_tasks_http_error() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .respond_with(ResponseTemplate::new(403).set_body_string("Forbidden"))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery::default();

        let result = get_tasks(&client, &query).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            IaError::Http { status, message } => {
                assert_eq!(status, 403);
                assert_eq!(message, "Forbidden");
            }
            other => panic!("expected Http error, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_get_tasks_requires_auth() {
        let mut config = crate::config::IaConfig::default();
        config.general.host = "localhost".to_string();
        config.general.secure = false;
        // No s3_access / s3_secret set
        let client = IaClient::from_config(config).unwrap();
        let query = TasksQuery::default();

        let result = get_tasks(&client, &query).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), IaError::Auth(_)));
    }

    #[tokio::test]
    async fn test_get_tasks_with_submitter_and_args() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("submitter", "user@example.com"))
            .and(query_param("args", "*s3-put*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": {
                    "summary": {
                        "queued": 5,
                        "running": 2,
                        "error": 1,
                        "paused": 0
                    }
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery {
            submitter: Some("user@example.com".to_string()),
            args: Some("*s3-put*".to_string()),
            ..Default::default()
        };

        let result = get_tasks(&client, &query).await.unwrap();
        assert_eq!(result.summary.queued, 5);
        assert_eq!(result.summary.running, 2);
        assert_eq!(result.summary.error, 1);
    }

    #[tokio::test]
    async fn test_get_tasks_success_false() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": false,
                "value": {
                    "summary": {
                        "queued": 0,
                        "running": 0,
                        "error": 0,
                        "paused": 0
                    }
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery::default();

        let result = get_tasks(&client, &query).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            IaError::Http { status, message } => {
                assert_eq!(status, 200);
                assert!(message.contains("success: false"));
            }
            other => panic!("expected Http error, got: {other}"),
        }
    }
}
