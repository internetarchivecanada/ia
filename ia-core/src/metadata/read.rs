use crate::client::IaClient;
use crate::error::{IaError, Result};
use crate::types::ItemMetadata;

/// Fetch full metadata for an item.
pub async fn get(client: &IaClient, identifier: &str) -> Result<ItemMetadata> {
    let url = client.url(&format!("/metadata/{identifier}"));

    let response = client.http().get(&url).send().await?;

    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(IaError::NotFound(identifier.to_string()));
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(IaError::Http {
            status: status.as_u16(),
            message: body,
        });
    }

    let item: ItemMetadata = response
        .json()
        .await
        .map_err(reqwest_middleware::Error::from)?;

    Ok(item)
}

/// Check if an item exists.
pub async fn exists(client: &IaClient, identifier: &str) -> Result<bool> {
    match get(client, identifier).await {
        Ok(_) => Ok(true),
        Err(IaError::NotFound(_)) => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn mock_config(server_uri: &str) -> crate::config::IaConfig {
        let mut config = crate::config::IaConfig::default();
        // Strip protocol prefix for host
        let host = server_uri
            .strip_prefix("http://")
            .or_else(|| server_uri.strip_prefix("https://"))
            .unwrap_or(server_uri);
        config.general.host = host.to_string();
        config.general.secure = false;
        config
    }

    #[tokio::test]
    async fn get_item_metadata() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/test-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metadata": {
                    "identifier": "test-item",
                    "title": "Test Item",
                    "mediatype": "texts"
                },
                "files": [
                    {"name": "test.pdf", "size": "1000", "source": "original", "md5": "abc123"}
                ],
                "server": "ia000000.us.archive.org"
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let item = get(&client, "test-item").await.unwrap();

        assert_eq!(item.metadata.identifier.as_deref(), Some("test-item"));
        assert_eq!(item.files.len(), 1);
        assert_eq!(item.files[0].name, "test.pdf");
        assert_eq!(item.files[0].size, Some(1000));
    }

    #[tokio::test]
    async fn get_item_not_found() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/nonexistent"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = get(&client, "nonexistent").await;

        assert!(matches!(result, Err(IaError::NotFound(_))));
    }

    #[tokio::test]
    async fn exists_returns_true_for_existing_item() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/nasa"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metadata": {"identifier": "nasa"},
                "files": []
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        assert!(exists(&client, "nasa").await.unwrap());
    }

    #[tokio::test]
    async fn exists_returns_false_for_missing_item() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/nope"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        assert!(!exists(&client, "nope").await.unwrap());
    }
}
