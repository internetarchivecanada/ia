//! Authentication with the Internet Archive.
//!
//! Handles login via the xauthn API, credential validation, and account info retrieval.

use serde::{Deserialize, Serialize};

use crate::client::IaClient;
use crate::error::{IaError, Result};

/// Credentials and account info returned by a successful login.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub s3_access: String,
    pub s3_secret: String,
    pub logged_in_user: String,
    pub logged_in_sig: String,
    pub screenname: String,
    pub itemname: Option<String>,
}

/// Account info returned by whoami/check operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    pub screenname: String,
    pub email: String,
    pub itemname: Option<String>,
}

/// Response from the IA xauthn login API.
#[derive(Debug, Deserialize)]
struct XauthnResponse {
    success: bool,
    values: Option<XauthnValues>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct XauthnValues {
    s3: Option<XauthnS3>,
    cookies: Option<XauthnCookies>,
    screenname: Option<String>,
    itemname: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct XauthnS3 {
    access: String,
    secret: String,
}

#[derive(Debug, Deserialize)]
struct XauthnCookies {
    #[serde(rename = "logged-in-user")]
    logged_in_user: String,
    #[serde(rename = "logged-in-sig")]
    logged_in_sig: String,
}

/// Response from the xauthn info endpoint.
#[derive(Debug, Deserialize)]
struct XauthnInfoResponse {
    success: bool,
    values: Option<XauthnInfoValues>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct XauthnInfoValues {
    screenname: Option<String>,
    itemname: Option<String>,
}

/// Validate S3 keys by calling the IA API.
///
/// Uses the xauthn info endpoint to verify credentials are valid.
/// Returns an [`AccountInfo`] on success.
pub async fn check_keys(client: &IaClient) -> Result<AccountInfo> {
    let (access, secret) = client.require_auth()?;
    let url = format!(
        "{}://{}/services/xauthn/?op=info",
        client.protocol(),
        client.host()
    );
    let resp = client
        .http()
        .post(&url)
        .header("user-agent", client.user_agent())
        .header("authorization", format!("LOW {access}:{secret}"))
        .send()
        .await
        .map_err(|e| IaError::Auth(format!("key check request failed: {e}")))?;

    let body = resp
        .text()
        .await
        .map_err(|e| IaError::Auth(format!("failed to read response: {e}")))?;

    let parsed: XauthnInfoResponse = serde_json::from_str(&body)
        .map_err(|e| IaError::Auth(format!("failed to parse response: {e}")))?;

    if !parsed.success {
        let msg = parsed
            .error
            .unwrap_or_else(|| "invalid credentials".into());
        return Err(IaError::Auth(msg));
    }

    let values = parsed.values.unwrap_or(XauthnInfoValues {
        screenname: None,
        itemname: None,
    });

    // Extract email from cookies in client config
    let email = client
        .config()
        .cookies
        .get("logged-in-user")
        .cloned()
        .unwrap_or_default();

    Ok(AccountInfo {
        screenname: values.screenname.unwrap_or_default(),
        email,
        itemname: values.itemname,
    })
}

/// Retrieve account info from archive.org (alias for `check_keys`).
///
/// Uses the same endpoint as [`check_keys`] but is semantically "who am I?"
pub async fn whoami(client: &IaClient) -> Result<AccountInfo> {
    check_keys(client).await
}

/// Authenticate with archive.org and return credentials.
///
/// POSTs to `/services/xauthn/?op=login` with email and password.
/// On success, returns S3 keys, cookies, and account info.
pub async fn login(client: &IaClient, email: &str, password: &str) -> Result<AuthConfig> {
    let url = format!(
        "{}://{}/services/xauthn/?op=login",
        client.protocol(),
        client.host()
    );
    let resp = client
        .http()
        .post(&url)
        .header("user-agent", client.user_agent())
        .form(&[("email", email), ("password", password)])
        .send()
        .await
        .map_err(|e| IaError::Auth(format!("login request failed: {e}")))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| IaError::Auth(format!("failed to read login response: {e}")))?;

    if !status.is_success() {
        return Err(IaError::Auth(format!(
            "login failed with HTTP {status}: {body}"
        )));
    }

    let parsed: XauthnResponse = serde_json::from_str(&body)
        .map_err(|e| IaError::Auth(format!("failed to parse login response: {e}")))?;

    if !parsed.success {
        let message = match parsed.values.as_ref().and_then(|v| v.reason.as_deref()) {
            Some("account_not_found") => "Account not found, check your email".to_string(),
            Some("account_bad_password") => "Incorrect password".to_string(),
            Some(reason) => format!("Login failed: {reason}"),
            None => parsed
                .error
                .unwrap_or_else(|| "Login failed (unknown error)".to_string()),
        };
        return Err(IaError::Auth(message));
    }

    let values = parsed
        .values
        .ok_or_else(|| IaError::Auth("login response missing values".into()))?;
    let s3 = values
        .s3
        .ok_or_else(|| IaError::Auth("login response missing S3 keys".into()))?;
    let cookies = values
        .cookies
        .ok_or_else(|| IaError::Auth("login response missing cookies".into()))?;

    Ok(AuthConfig {
        s3_access: s3.access,
        s3_secret: s3.secret,
        logged_in_user: cookies.logged_in_user,
        logged_in_sig: cookies.logged_in_sig,
        screenname: values.screenname.unwrap_or_default(),
        itemname: values.itemname,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Helper: build an IaClient pointing at a mock server.
    async fn test_client(host: &str) -> crate::client::IaClient {
        let mut config = crate::config::IaConfig::default();
        config.general.host = host.to_string();
        config.general.secure = false; // mock server is HTTP
        crate::client::IaClient::from_config(config).unwrap()
    }

    #[tokio::test]
    async fn login_success() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "success": true,
            "values": {
                "s3": {"access": "test-access", "secret": "test-secret"},
                "cookies": {
                    "logged-in-user": "user%40example.com",
                    "logged-in-sig": "test-sig"
                },
                "screenname": "testuser",
                "itemname": "@testuser"
            }
        });
        Mock::given(method("POST"))
            .and(path("/services/xauthn/"))
            .and(query_param("op", "login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        let host = server.uri().replace("http://", "");
        let client = test_client(&host).await;
        let result = login(&client, "user@example.com", "password123").await;

        let auth = result.unwrap();
        assert_eq!(auth.s3_access, "test-access");
        assert_eq!(auth.s3_secret, "test-secret");
        assert_eq!(auth.logged_in_user, "user%40example.com");
        assert_eq!(auth.logged_in_sig, "test-sig");
        assert_eq!(auth.screenname, "testuser");
        assert_eq!(auth.itemname.as_deref(), Some("@testuser"));
    }

    #[tokio::test]
    async fn login_account_not_found() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "success": false,
            "values": {"reason": "account_not_found"}
        });
        Mock::given(method("POST"))
            .and(path("/services/xauthn/"))
            .and(query_param("op", "login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        let host = server.uri().replace("http://", "");
        let client = test_client(&host).await;
        let result = login(&client, "bad@example.com", "pass").await;

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Account not found"), "got: {msg}");
    }

    #[tokio::test]
    async fn login_bad_password() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "success": false,
            "values": {"reason": "account_bad_password"}
        });
        Mock::given(method("POST"))
            .and(path("/services/xauthn/"))
            .and(query_param("op", "login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        let host = server.uri().replace("http://", "");
        let client = test_client(&host).await;
        let result = login(&client, "user@example.com", "wrong").await;

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Incorrect password"), "got: {msg}");
    }

    #[tokio::test]
    async fn check_keys_valid() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "success": true,
            "values": {
                "screenname": "testuser",
                "itemname": "@testuser"
            }
        });
        Mock::given(method("POST"))
            .and(path("/services/xauthn/"))
            .and(query_param("op", "info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        let host = server.uri().replace("http://", "");
        let mut config = crate::config::IaConfig::default();
        config.general.host = host;
        config.general.secure = false;
        config.s3_access = Some("test-access".into());
        config.s3_secret = Some("test-secret".into());
        let client = IaClient::from_config(config).unwrap();

        let info = check_keys(&client).await.unwrap();
        assert_eq!(info.screenname, "testuser");
    }

    #[tokio::test]
    async fn check_keys_invalid() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "success": false,
            "error": "invalid credentials"
        });
        Mock::given(method("POST"))
            .and(path("/services/xauthn/"))
            .and(query_param("op", "info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        let host = server.uri().replace("http://", "");
        let mut config = crate::config::IaConfig::default();
        config.general.host = host;
        config.general.secure = false;
        config.s3_access = Some("bad-access".into());
        config.s3_secret = Some("bad-secret".into());
        let client = IaClient::from_config(config).unwrap();

        let result = check_keys(&client).await;
        assert!(result.is_err());
    }
}
