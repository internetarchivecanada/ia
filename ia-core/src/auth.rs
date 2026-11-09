//! Authentication with the Internet Archive.
//!
//! Handles login via the xauthn API, credential validation, and account info retrieval.

use std::path::Path;

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
        let msg = parsed.error.unwrap_or_else(|| "invalid credentials".into());
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

/// Build the `Authorization: LOW access:secret` header value from config.
///
/// Returns `None` if S3 credentials are not configured.
#[must_use]
pub fn s3_auth_value(config: &crate::config::IaConfig) -> Option<String> {
    let access = config.s3_access.as_deref()?;
    let secret = config.s3_secret.as_deref()?;
    Some(format!("LOW {access}:{secret}"))
}

/// The `login` and `password` of one netrc entry.
#[derive(Debug, Default)]
struct NetrcEntry {
    login: Option<String>,
    password: Option<String>,
}

/// Which entry the netrc tokens being read belong to.
#[derive(Debug, Clone, Copy, PartialEq)]
enum NetrcTarget {
    /// Before the first `machine` or `default` token.
    None,
    /// The first `machine archive.org` entry.
    Archive,
    /// Any other entry, including `default` and a repeated
    /// `machine archive.org`.
    Other,
}

/// A netrc keyword whose value is the next token.
#[derive(Debug, Clone, Copy, PartialEq)]
enum NetrcKey {
    Machine,
    Login,
    Password,
    Account,
}

/// The netrc keywords that start an entry or a macro. Unquoted, these are
/// never read as values: a keyword left without its value would otherwise
/// take one of them, and the parser would read the entry that follows as
/// part of the current one. Other keywords taken as a value leave the next
/// token where a keyword is expected, which is already an error.
const NETRC_ENTRY_KEYWORDS: [&str; 3] = ["machine", "default", "macdef"];

/// Whether an unquoted value looks like `machine`, `default` or `macdef`:
/// equal to one without regard to case, once any Unicode whitespace or
/// byte order mark at either end is removed. A value like this most likely
/// means the keyword before it has no value and a new entry starts here.
fn is_entry_keyword_like(text: &str) -> bool {
    let word = text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    NETRC_ENTRY_KEYWORDS
        .iter()
        .any(|keyword| word.eq_ignore_ascii_case(keyword))
}

/// One token of a netrc line.
#[derive(Debug, PartialEq)]
struct NetrcToken {
    text: String,
    /// Whether the token was written in double quotes. A quoted token is
    /// always a value, never a keyword or a comment.
    quoted: bool,
}

/// Read the next token from one netrc line, or `None` at the end of it.
///
/// Tokens are separated by ASCII whitespace. A token that starts with `"` runs to
/// the closing `"` and may contain whitespace; inside it a backslash makes
/// the next character literal. Other tokens are taken as written.
fn next_netrc_token(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    line_no: usize,
) -> Result<Option<NetrcToken>> {
    while chars.next_if(char::is_ascii_whitespace).is_some() {}
    match chars.peek() {
        None => return Ok(None),
        Some('"') => {
            chars.next();
        }
        Some(_) => {
            let text = std::iter::from_fn(|| chars.next_if(|c| !c.is_ascii_whitespace())).collect();
            return Ok(Some(NetrcToken {
                text,
                quoted: false,
            }));
        }
    }
    let mut text = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Ok(Some(NetrcToken { text, quoted: true })),
            '\\' => match chars.next() {
                Some(escaped) => text.push(escaped),
                None => break,
            },
            other => text.push(other),
        }
    }
    Err(IaError::Auth(format!(
        "netrc file line {line_no}: a quoted value has no closing quote"
    )))
}

/// Parse a netrc file and extract archive.org credentials.
///
/// Follows the netrc format of ftp(1): tokens are separated by ASCII
/// whitespace, line breaks included, and an entry runs from its `machine`
/// or `default` token to the next one. The first `machine archive.org`
/// entry (host name compared without regard to case) is used, and its
/// `login` (or `user`) and `password` must both be present. A `default`
/// entry is parsed and skipped like any other entry: without a
/// `machine archive.org` entry the file is an error, as in the Python `ia`
/// client, so no password meant for other hosts is sent to archive.org.
///
/// A value may be written in double quotes to include whitespace, with a
/// backslash escaping the next character. `account` values are skipped, and
/// so are each `macdef` body (the lines up to the next empty line) and each
/// comment (a keyword-position token starting with `#`, to the end of its
/// line). Any other token where a keyword is expected is an error, and so is
/// an unquoted `machine`, `default` or `macdef` (in any case) where a value
/// is expected, so a keyword left without its value cannot pull a value
/// from the next entry. An empty login or password counts as missing.
pub fn parse_netrc(path: &Path) -> Result<(String, String)> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| IaError::Auth(format!("failed to read netrc file: {e}")))?;
    // A leading byte order mark is not a token. CRLF and lone CR line
    // endings both end a line.
    let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
    let content = content.replace("\r\n", "\n").replace('\r', "\n");

    let mut archive: Option<NetrcEntry> = None;
    let mut target = NetrcTarget::None;
    let mut pending: Option<NetrcKey> = None;
    let mut in_macdef = false;

    for (index, line) in content.split('\n').enumerate() {
        let line_no = index + 1;
        if in_macdef {
            if line.is_empty() {
                in_macdef = false;
            }
            continue;
        }
        let mut chars = line.chars().peekable();
        while let Some(token) = next_netrc_token(&mut chars, line_no)? {
            if let Some(key) = pending.take() {
                // `machine`, `default` or `macdef` here means the previous
                // keyword has no value. Taking it as the value would let the
                // entry that follows overwrite this one, so it is an error;
                // a value that is one of these words must be quoted.
                if !token.quoted && is_entry_keyword_like(&token.text) {
                    return Err(IaError::Auth(format!(
                        "netrc file line {line_no}: expected a value, found the keyword `{}`; \
                         write a value that is a keyword in double quotes",
                        token.text
                    )));
                }
                match key {
                    NetrcKey::Machine => {
                        let is_archive = token.text.eq_ignore_ascii_case("archive.org");
                        target = if is_archive && archive.is_none() {
                            archive = Some(NetrcEntry::default());
                            NetrcTarget::Archive
                        } else {
                            NetrcTarget::Other
                        };
                    }
                    NetrcKey::Login | NetrcKey::Password => {
                        let entry = match target {
                            NetrcTarget::Archive => archive.as_mut(),
                            NetrcTarget::None | NetrcTarget::Other => None,
                        };
                        if let Some(entry) = entry {
                            if key == NetrcKey::Login {
                                entry.login = Some(token.text);
                            } else {
                                entry.password = Some(token.text);
                            }
                        }
                    }
                    NetrcKey::Account => {}
                }
                continue;
            }
            let keyword = if token.quoted {
                ""
            } else {
                token.text.as_str()
            };
            match keyword {
                "machine" => pending = Some(NetrcKey::Machine),
                "login" | "user" => pending = Some(NetrcKey::Login),
                "password" => pending = Some(NetrcKey::Password),
                "account" => pending = Some(NetrcKey::Account),
                "default" => target = NetrcTarget::Other,
                "macdef" => {
                    // The rest of this line is the macro name; the body
                    // runs to the next empty line.
                    in_macdef = true;
                    break;
                }
                // A comment runs to the end of the line. Only a token in
                // keyword position starts one, so a value such as a
                // password beginning with `#` is still read as a value.
                comment if comment.starts_with('#') => break,
                // The token is not named: it may be part of a password.
                _ => {
                    return Err(IaError::Auth(format!(
                        "netrc file line {line_no}: expected a keyword \
                         (machine, default, login, password, account, macdef)"
                    )));
                }
            }
        }
    }

    let entry =
        archive.ok_or_else(|| IaError::Auth("no archive.org entry found in netrc file".into()))?;
    // An empty value (`""`) counts as missing.
    let non_empty = |value: Option<String>| value.filter(|v| !v.is_empty());
    match (non_empty(entry.login), non_empty(entry.password)) {
        (Some(login), Some(password)) => Ok((login, password)),
        (None, _) => Err(IaError::Auth(
            "the netrc entry `machine archive.org` has no login".into(),
        )),
        (Some(_), None) => Err(IaError::Auth(
            "the netrc entry `machine archive.org` has no password".into(),
        )),
    }
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

    #[test]
    fn parse_netrc_success() {
        let dir = tempfile::tempdir().unwrap();
        let netrc_path = dir.path().join(".netrc");
        std::fs::write(
            &netrc_path,
            "machine archive.org\n  login user@example.com\n  password secret123\n",
        )
        .unwrap();

        let (email, password) = parse_netrc(&netrc_path).unwrap();
        assert_eq!(email, "user@example.com");
        assert_eq!(password, "secret123");
    }

    #[test]
    fn parse_netrc_missing_host() {
        let dir = tempfile::tempdir().unwrap();
        let netrc_path = dir.path().join(".netrc");
        std::fs::write(
            &netrc_path,
            "machine github.com\n  login user\n  password pass\n",
        )
        .unwrap();

        let result = parse_netrc(&netrc_path);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("archive.org"), "got: {msg}");
    }

    /// Write `content` to a temporary netrc file and parse it.
    fn parse_netrc_str(content: &str) -> Result<(String, String)> {
        let dir = tempfile::tempdir().unwrap();
        let netrc_path = dir.path().join(".netrc");
        std::fs::write(&netrc_path, content).unwrap();
        parse_netrc(&netrc_path)
    }

    fn right() -> (String, String) {
        ("right@example.com".to_string(), "rightpass".to_string())
    }

    #[test]
    fn parse_netrc_one_token_per_line_archive_first() {
        let got = parse_netrc_str(
            "machine archive.org\nlogin right@example.com\npassword rightpass\n\
             machine example.com\nlogin wrong@example.com\npassword wrongpass\n",
        )
        .unwrap();
        assert_eq!(got, right());
    }

    #[test]
    fn parse_netrc_one_token_per_line_archive_last() {
        let got = parse_netrc_str(
            "machine example.com\nlogin wrong@example.com\npassword wrongpass\n\
             machine archive.org\nlogin right@example.com\npassword rightpass\n",
        )
        .unwrap();
        assert_eq!(got, right());
    }

    #[test]
    fn parse_netrc_every_word_on_its_own_line() {
        let got = parse_netrc_str(
            "machine\narchive.org\nlogin\nright@example.com\npassword\nrightpass\n\
             machine\nexample.com\nlogin\nwrong@example.com\npassword\nwrongpass\n",
        )
        .unwrap();
        assert_eq!(got, right());
    }

    #[test]
    fn parse_netrc_one_line_per_machine_archive_first() {
        let got = parse_netrc_str(
            "machine archive.org login right@example.com password rightpass\n\
             machine example.com login wrong@example.com password wrongpass\n",
        )
        .unwrap();
        assert_eq!(got, right());
    }

    #[test]
    fn parse_netrc_one_line_per_machine_archive_last() {
        let got = parse_netrc_str(
            "machine example.com login wrong@example.com password wrongpass\n\
             machine archive.org login right@example.com password rightpass\n",
        )
        .unwrap();
        assert_eq!(got, right());
    }

    #[test]
    fn parse_netrc_all_entries_on_one_line() {
        let got = parse_netrc_str(
            "machine archive.org login right@example.com password rightpass \
             machine example.com login wrong@example.com password wrongpass\n",
        )
        .unwrap();
        assert_eq!(got, right());
    }

    #[test]
    fn parse_netrc_default_entry_is_ignored_archive_entry_required() {
        let msg = parse_netrc_str(
            "machine example.com login wrong@example.com password wrongpass\n\
             default login other@example.com password otherpass\n",
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            msg,
            "authentication required: no archive.org entry found in netrc file"
        );
    }

    #[test]
    fn parse_netrc_archive_entry_preferred_over_default() {
        let got = parse_netrc_str(
            "machine archive.org\nlogin right@example.com\npassword rightpass\n\
             default\nlogin wrong@example.com\npassword wrongpass\n",
        )
        .unwrap();
        assert_eq!(got, right());
    }

    #[test]
    fn parse_netrc_first_archive_entry_wins() {
        let got = parse_netrc_str(
            "machine archive.org login right@example.com password rightpass\n\
             machine archive.org login wrong@example.com password wrongpass\n",
        )
        .unwrap();
        assert_eq!(got, right());
    }

    #[test]
    fn parse_netrc_incomplete_archive_entry_does_not_borrow_from_others() {
        let result = parse_netrc_str(
            "machine archive.org\nlogin right@example.com\n\
             machine example.com\nlogin wrong@example.com\npassword wrongpass\n\
             default login other@example.com password otherpass\n",
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("`machine archive.org` has no password"),
            "got: {msg}"
        );
    }

    #[test]
    fn parse_netrc_missing_login_is_named() {
        let msg = parse_netrc_str("machine archive.org password rightpass\n")
            .unwrap_err()
            .to_string();
        assert!(
            msg.contains("`machine archive.org` has no login"),
            "got: {msg}"
        );
    }

    #[test]
    fn parse_netrc_trailing_keyword_without_value() {
        let msg = parse_netrc_str("machine archive.org login right@example.com password")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("has no password"), "got: {msg}");
    }

    #[test]
    fn parse_netrc_keyword_without_value_cannot_take_next_entry() {
        for content in [
            "machine archive.org login right password\n\
             machine example.com login wrong password wrongpass\n",
            "machine archive.org login\nmachine example.com login wrong password wrongpass\n",
            "default login\nmachine example.com login wrong password wrongpass\n",
            // The keyword after the dangling one is `default` or `machine`
            // itself: it must not be taken as the value.
            "machine archive.org login right password\ndefault login wrong password wrongpass\n",
            "machine archive.org login right password rightpass account\n\
             default login wrong password wrongpass\n",
            "machine archive.org login right password rightpass login\n\
             default login wrong password wrongpass\n",
            "default login right password\ndefault login wrong password wrongpass\n",
            "machine archive.org login right password\n\
             machine default login wrong password wrongpass\n",
            "machine archive.org login right password macdef\nx\n\n",
            // The same, with the entry keyword in another case or glued to
            // a non-ASCII space or byte order mark.
            "machine archive.org login right password\nDEFAULT login wrong password wrongpass\n",
            "machine archive.org login\nDefault login wrong password wrongpass\n",
            "machine archive.org login right password\n\
             \u{feff}default login wrong password wrongpass\n",
            "machine archive.org login right password\n\
             \u{a0}default login wrong password wrongpass\n",
            "machine archive.org login right password\nMachine\u{a0} example.com login wrong password wrongpass\n",
        ] {
            let result = parse_netrc_str(content);
            let msg = match &result {
                Err(e) => e.to_string(),
                Ok(_) => panic!("{content:?} gave {result:?}"),
            };
            assert!(
                msg.contains("line") || msg.contains("has no"),
                "{content:?} gave {msg}"
            );
        }
    }

    #[test]
    fn parse_netrc_quoted_keyword_is_a_value() {
        let got =
            parse_netrc_str("machine archive.org login right@example.com password \"default\"\n")
                .unwrap();
        assert_eq!(got.1, "default");
    }

    #[test]
    fn parse_netrc_unknown_token_is_an_error() {
        let result = parse_netrc_str(
            "machine archive.org login right@example.com password rightpass\n\
             host example.com login wrong@example.com password wrongpass\n",
        );
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("line 2"), "got: {msg}");
        assert!(!msg.contains("host"), "must not echo the token: {msg}");
        assert!(
            !msg.contains("example.com"),
            "must not echo file content: {msg}"
        );
    }

    #[test]
    fn parse_netrc_host_name_case_is_ignored() {
        let got = parse_netrc_str(
            "machine Archive.ORG login right@example.com password rightpass\n\
             default login wrong@example.com password wrongpass\n",
        )
        .unwrap();
        assert_eq!(got, right());
    }

    #[test]
    fn parse_netrc_user_is_an_alias_of_login() {
        let got =
            parse_netrc_str("machine archive.org user right@example.com password rightpass\n")
                .unwrap();
        assert_eq!(got, right());
    }

    #[test]
    fn parse_netrc_non_ascii_whitespace_is_part_of_a_value() {
        let got =
            parse_netrc_str("machine archive.org login right@example.com password pa\u{a0}ss\n")
                .unwrap();
        assert_eq!(got.1, "pa\u{a0}ss");
    }

    #[test]
    fn parse_netrc_leading_byte_order_mark() {
        let got = parse_netrc_str(
            "\u{feff}machine archive.org login right@example.com password rightpass\n",
        )
        .unwrap();
        assert_eq!(got, right());
    }

    #[test]
    fn parse_netrc_empty_quoted_values_are_missing() {
        let msg = parse_netrc_str("machine archive.org login \"\" password \"\"\n")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("has no login"), "got: {msg}");
        let msg = parse_netrc_str("machine archive.org login right password \"\"\n")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("has no password"), "got: {msg}");
    }

    #[test]
    fn parse_netrc_quoted_values() {
        let got = parse_netrc_str(
            "machine archive.org login \"right@example.com\" password \"pa ss \\\"q\\\\\"\n",
        )
        .unwrap();
        assert_eq!(got.0, "right@example.com");
        assert_eq!(got.1, "pa ss \"q\\");
    }

    #[test]
    fn parse_netrc_quoted_keyword_is_not_a_keyword() {
        let result =
            parse_netrc_str("machine archive.org login right@example.com \"password\" rightpass\n");
        assert!(result.is_err(), "got {result:?}");
    }

    #[test]
    fn parse_netrc_unterminated_quote_is_an_error() {
        let msg = parse_netrc_str("machine archive.org login right password \"rightpass\n")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("closing quote"), "got: {msg}");
    }

    #[test]
    fn parse_netrc_crlf_and_cr_line_endings() {
        let got = parse_netrc_str(
            "macdef init\r\ncd /pub\r\n\r\n\
             machine archive.org\r\nlogin right@example.com\r\npassword rightpass\r\n",
        )
        .unwrap();
        assert_eq!(got, right());

        let got = parse_netrc_str(
            "# comment\rmachine archive.org\rlogin right@example.com\rpassword rightpass\r",
        )
        .unwrap();
        assert_eq!(got, right());
    }

    #[test]
    fn parse_netrc_macdef_ends_only_at_empty_line() {
        let got = parse_netrc_str(
            "macdef init\n  \nmachine archive.org login wrong@example.com password wrongpass\n\n\
             machine archive.org login right@example.com password rightpass\n",
        )
        .unwrap();
        assert_eq!(got, right());
    }

    #[test]
    fn parse_netrc_macdef_at_end_of_file() {
        let got = parse_netrc_str(
            "machine archive.org login right@example.com password rightpass\n\
             macdef init\ncd /pub",
        )
        .unwrap();
        assert_eq!(got, right());
    }

    #[test]
    fn parse_netrc_default_entries_are_ignored() {
        let msg = parse_netrc_str(
            "default login wrong@example.com password wrongpass\n\
             default login other@example.com password otherpass\n",
        )
        .unwrap_err()
        .to_string();
        assert!(msg.contains("no archive.org entry"), "got: {msg}");
    }

    #[test]
    fn parse_netrc_default_before_machines() {
        let got = parse_netrc_str(
            "default login wrong@example.com password wrongpass\n\
             machine archive.org login right@example.com password rightpass\n",
        )
        .unwrap();
        assert_eq!(got, right());
    }

    #[test]
    fn parse_netrc_incomplete_default_entry_is_ignored() {
        let msg = parse_netrc_str("default login right@example.com\n")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("no archive.org entry"), "got: {msg}");
    }

    #[test]
    fn parse_netrc_account_value_is_not_read_as_keyword() {
        let got = parse_netrc_str(
            "machine archive.org account password login right@example.com password rightpass\n",
        )
        .unwrap();
        assert_eq!(got, right());
    }

    #[test]
    fn parse_netrc_macdef_body_is_skipped() {
        let got = parse_netrc_str(
            "machine archive.org login right@example.com password rightpass\n\
             macdef init\n\
             machine example.com login wrong@example.com password wrongpass\n\
             \n\
             machine other.example.com login x password y\n",
        )
        .unwrap();
        assert_eq!(got, right());

        let got = parse_netrc_str(
            "macdef init\nmachine archive.org login wrong@example.com password wrongpass\n\n\
             machine archive.org login right@example.com password rightpass\n",
        )
        .unwrap();
        assert_eq!(got, right());
    }

    #[test]
    fn parse_netrc_comment_lines_are_skipped() {
        let got = parse_netrc_str(
            "# machine archive.org login wrong@example.com password wrongpass\n\
             machine archive.org login right@example.com # old login wrong@example.com\n\
             password rightpass\n",
        )
        .unwrap();
        assert_eq!(got, right());
    }

    #[test]
    fn parse_netrc_password_may_start_with_hash() {
        let got = parse_netrc_str("machine archive.org login right@example.com password #pass\n")
            .unwrap();
        assert_eq!(got.1, "#pass");
    }

    #[test]
    fn parse_netrc_no_archive_and_no_default_entry() {
        let result =
            parse_netrc_str("machine example.com\nlogin wrong@example.com\npassword wrongpass\n");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("archive.org"), "got: {msg}");
    }

    #[test]
    fn parse_netrc_file_not_found() {
        let result = parse_netrc(Path::new("/nonexistent/.netrc"));
        assert!(result.is_err());
    }
}
