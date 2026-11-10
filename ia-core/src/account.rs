//! Look up, lock and unlock archive.org accounts.
//!
//! The account API (`/services/xauthn/`) is admin-only. Requests carry the
//! `logged-in-user` and `logged-in-sig` cookies from the `[cookies]` config
//! section, as the Python client sends them, and no `Authorization` header.
//! The cookies are sent only over `https` to `archive.org` and its
//! subdomains, or to loopback hosts; see [`cookie_header`].
//!
//! A lookup answers with the account's `values` object, kept whole in
//! [`Account::values`]. A lock or unlock is sent with the item name a lookup
//! returned. Both requests go out without following redirects and without
//! a retry layer.

use std::fmt;

use reqwest::header::{COOKIE, LOCATION};
use reqwest::StatusCode;
use serde_json::Value;

use crate::client::IaClient;
use crate::error::{IaError, Result};

/// The account API path on the configured host.
const XAUTHN_PATH: &str = "/services/xauthn/";

/// The wait used when a 429 carries no usable `Retry-After`, as for
/// metadata writes.
const DEFAULT_RETRY_AFTER_SECS: u64 = 30;

/// Which form field a lookup sends: the account's email address, screen
/// name or item name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LookupType {
    /// An email address (`email=…`).
    Email,
    /// A screen name (`screenname=…`).
    Screenname,
    /// An item name, which starts with `@` (`itemname=@…`).
    Itemname,
}

impl LookupType {
    /// The lookup type for a value, in the Python client's order: a leading
    /// `@` is an item name; otherwise an email address (see [`is_email`]);
    /// otherwise a screen name.
    ///
    /// ```
    /// use ia_core::account::LookupType;
    /// assert_eq!(LookupType::detect("@example-user"), LookupType::Itemname);
    /// assert_eq!(LookupType::detect("person@example.com"), LookupType::Email);
    /// assert_eq!(LookupType::detect("example-user-1"), LookupType::Screenname);
    /// ```
    #[must_use]
    pub fn detect(value: &str) -> Self {
        if value.starts_with('@') {
            Self::Itemname
        } else if is_email(value) {
            Self::Email
        } else {
            Self::Screenname
        }
    }

    /// The form field name the lookup sends: `email`, `screenname` or
    /// `itemname`.
    #[must_use]
    pub fn field(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Screenname => "screenname",
            Self::Itemname => "itemname",
        }
    }
}

impl fmt::Display for LookupType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.field())
    }
}

/// Whether a value is an email address: a local part of one or more of
/// `A-Z a-z 0-9 _ . + -`, one `@`, then two or more dot-separated labels of
/// `A-Z a-z 0-9 -` that neither start nor end with `-`, the last two or
/// more ASCII letters.
///
/// ```
/// use ia_core::account::is_email;
/// assert!(is_email("First.Last+tag@mail.example.org"));
/// assert!(!is_email("person@localhost"));
/// assert!(!is_email("a@-b.example.com"));
/// ```
#[must_use]
pub fn is_email(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    let local_ok = !local.is_empty()
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '-'));
    let labels: Vec<&str> = domain.split('.').collect();
    let Some((last, rest)) = labels.split_last() else {
        return false;
    };
    let label_ok = |label: &&str| {
        !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    local_ok
        && !rest.is_empty()
        && rest.iter().all(label_ok)
        && last.len() >= 2
        && last.chars().all(|c| c.is_ascii_alphabetic())
}

/// An account as a lookup returned it.
#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    /// How the account was looked up.
    pub lookup: LookupType,
    /// The `values` object of the answer, as received.
    pub values: serde_json::Map<String, Value>,
}

impl Account {
    /// A field of [`Self::values`], when it is a string.
    fn str_field(&self, name: &str) -> Option<&str> {
        self.values.get(name).and_then(Value::as_str)
    }

    /// The `email` field, when it is a string.
    #[must_use]
    pub fn email(&self) -> Option<&str> {
        self.str_field("email")
    }

    /// The `canonical_email` field, when it is a string.
    #[must_use]
    pub fn canonical_email(&self) -> Option<&str> {
        self.str_field("canonical_email")
    }

    /// The `screenname` field, when it is a string.
    #[must_use]
    pub fn screenname(&self) -> Option<&str> {
        self.str_field("screenname")
    }

    /// The `itemname` field, when it is a string.
    #[must_use]
    pub fn itemname(&self) -> Option<&str> {
        self.str_field("itemname")
    }

    /// The `locked` field, when it is a boolean.
    #[must_use]
    pub fn locked(&self) -> Option<bool> {
        self.values.get("locked").and_then(Value::as_bool)
    }

    /// Whether this answer is the account `value` was looked up by,
    /// ignoring letter case: an email lookup matches `email` or
    /// `canonical_email`, a screen-name lookup `screenname`, an item-name
    /// lookup `itemname`.
    fn answers(&self, value: &str) -> bool {
        let wanted = value.to_lowercase();
        let matches = |field: Option<&str>| field.is_some_and(|f| f.to_lowercase() == wanted);
        match self.lookup {
            LookupType::Email => matches(self.email()) || matches(self.canonical_email()),
            LookupType::Screenname => matches(self.screenname()),
            LookupType::Itemname => matches(self.itemname()),
        }
    }
}

/// Look an account up, with the lookup type detected from `user` (see
/// [`LookupType::detect`]).
///
/// Fails with [`IaError::AccountApi`] when the account is not found, the
/// answer is not JSON, or the answer is for a different account;
/// [`IaError::RateLimited`] on a 429; [`IaError::Http`] on a redirect or
/// another non-2xx status; [`IaError::Auth`] before any request when
/// [`cookie_header`] fails.
pub async fn lookup(client: &IaClient, user: &str) -> Result<Account> {
    lookup_by(client, LookupType::detect(user), user).await
}

/// Look an account up by the given field. Errors as for [`lookup`].
pub async fn lookup_by(client: &IaClient, lookup: LookupType, value: &str) -> Result<Account> {
    let cookie = cookie_header(client)?;
    let response = post(client, "info", &cookie, &[(lookup.field(), value)]).await?;
    let body = response
        .text()
        .await
        .map_err(|e| IaError::Network(reqwest_middleware::Error::Reqwest(e)))?;
    let api_error = |message: String| IaError::AccountApi {
        account: value.to_string(),
        message,
    };

    let parsed: Value = serde_json::from_str(&body)
        .map_err(|_| api_error(format!("response is not JSON: {body}")))?;
    if let Some(message) = error_text(&parsed) {
        return Err(api_error(message));
    }
    let values = match parsed.get("values") {
        Some(Value::Object(values)) if !values.is_empty() && !is_success_false(&parsed) => {
            values.clone()
        }
        _ => return Err(api_error("account not found".into())),
    };

    let account = Account { lookup, values };
    if !account.answers(value) {
        return Err(api_error("lookup answered with a different account".into()));
    }
    Ok(account)
}

/// Lock (`locked = true`) or unlock an account by its item name, recording
/// `comment` when it is non-empty. Returns the answer parsed as JSON, or a
/// JSON string holding the body when it is not JSON.
///
/// The item name must start with `@` and have at least one more character,
/// else [`IaError::AccountApi`] with no request. A 2xx JSON answer with
/// `"success": false` or a truthy `error` is [`IaError::AccountApi`]; any
/// other 2xx answer is success. A 5xx is returned as [`IaError::Http`]
/// after one request and is never replayed: the server may already have
/// acted. Errors in `AccountApi` name `itemname` as the account.
pub async fn set_locked(
    client: &IaClient,
    itemname: &str,
    locked: bool,
    comment: Option<&str>,
) -> Result<Value> {
    let cookie = cookie_header(client)?;
    let api_error = |message: String| IaError::AccountApi {
        account: itemname.to_string(),
        message,
    };
    if !itemname.starts_with('@') || itemname.len() < 2 {
        return Err(api_error("lookup returned no usable itemname".into()));
    }

    let mut form = vec![
        ("itemname", itemname),
        ("is_lock", if locked { "1" } else { "0" }),
    ];
    if let Some(comment) = comment.filter(|c| !c.is_empty()) {
        form.push(("comments", comment));
    }
    let response = post(client, "lock_unlock", &cookie, &form).await?;
    let body = response
        .text()
        .await
        .map_err(|e| IaError::Network(reqwest_middleware::Error::Reqwest(e)))?;

    let Ok(parsed) = serde_json::from_str::<Value>(&body) else {
        return Ok(Value::String(body));
    };
    if let Some(message) = error_text(&parsed) {
        return Err(api_error(message));
    }
    if is_success_false(&parsed) {
        let refused = if locked {
            "lock refused"
        } else {
            "unlock refused"
        };
        return Err(api_error(refused.into()));
    }
    Ok(parsed)
}

/// Lock an account; see [`set_locked`].
pub async fn lock(client: &IaClient, itemname: &str, comment: Option<&str>) -> Result<Value> {
    set_locked(client, itemname, true, comment).await
}

/// Unlock an account; see [`set_locked`].
pub async fn unlock(client: &IaClient, itemname: &str, comment: Option<&str>) -> Result<Value> {
    set_locked(client, itemname, false, comment).await
}

/// The `Cookie` header value for account requests:
/// `logged-in-user=<v>; logged-in-sig=<v>`, each value as stored, cut at its
/// first `;` and trimmed.
///
/// Fails with [`crate::IaError::Auth`] when either cookie is missing or
/// empty, or when the request URL would not be `https` to `archive.org` or
/// a subdomain of it, or any scheme to a loopback host (`localhost`,
/// `127.0.0.1`, `[::1]`, any port), or would carry a user name or password.
pub fn cookie_header(client: &IaClient) -> Result<String> {
    let cookie = |name: &str| {
        client
            .config()
            .cookies
            .get(name)
            .map(|v| v.split(';').next().unwrap_or_default().trim())
            .filter(|v| !v.is_empty())
    };
    let (Some(user), Some(sig)) = (cookie("logged-in-user"), cookie("logged-in-sig")) else {
        return Err(IaError::Auth(
            "ia account needs the logged-in-user and logged-in-sig cookies; run `ia config login`"
                .into(),
        ));
    };
    check_cookie_target(client)?;
    Ok(format!("logged-in-user={user}; logged-in-sig={sig}"))
}

/// Check that the account API URL may receive the login cookies: `https`
/// to `archive.org` or a subdomain of it, or any scheme to a loopback host,
/// with no user name or password.
///
/// The URL is parsed, so a configured host string that carries a path,
/// user information or other parts is judged by the host it really names.
/// The Python client always sends these cookies over `https`; plain `http`
/// is allowed only to loopback hosts, for tests and local servers. User
/// information is refused because reqwest would turn it into an
/// `Authorization: Basic` header.
fn check_cookie_target(client: &IaClient) -> Result<()> {
    let refuse = |reason: String| {
        Err(IaError::Auth(format!(
            "ia account sends login cookies only to archive.org over https, \
             or to a loopback host; {reason}"
        )))
    };
    let host_is = || format!("host is {}", client.host());
    let Ok(url) = reqwest::Url::parse(&client.url(XAUTHN_PATH)) else {
        return refuse(host_is());
    };
    // Checked first, and the host string not echoed: it holds the password.
    if !url.username().is_empty() || url.password().is_some_and(|p| !p.is_empty()) {
        return refuse("the host names a user or password".into());
    }
    let Some(host) = url.host_str() else {
        return refuse(host_is());
    };
    // A trailing dot names the same host (a fully qualified domain name).
    let host = host.to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    if matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
        return Ok(());
    }
    if host != "archive.org" && !host.ends_with(".archive.org") {
        return refuse(host_is());
    }
    if url.scheme() != "https" {
        return refuse(format!("{} uses {}", client.host(), url.scheme()));
    }
    Ok(())
}

/// POST a form to `/services/xauthn/?op=<op>` with the login cookies,
/// without following redirects and without a retry layer, and map a 429,
/// a redirect and any other non-2xx status to errors.
async fn post(
    client: &IaClient,
    op: &str,
    cookie: &str,
    form: &[(&str, &str)],
) -> Result<reqwest::Response> {
    let url = client.url(&format!("{XAUTHN_PATH}?op={op}"));
    let request = client
        .no_redirect_http()
        .post(&url)
        .header(COOKIE, cookie)
        .form(form);
    let response = crate::retry::send_recorded(client.retry_stats(), request)
        .await
        .map_err(|e| IaError::Network(reqwest_middleware::Error::Reqwest(e)))?;

    let status = response.status();
    if status == StatusCode::TOO_MANY_REQUESTS {
        let retry_after = crate::retry::extract_retry_after(response.headers());
        return Err(IaError::RateLimited {
            // At least one second: a `Retry-After` of 0 or a past date would
            // have callers that resend on a 429 do so with no wait at all.
            retry_after: retry_after.unwrap_or(DEFAULT_RETRY_AFTER_SECS).max(1),
        });
    }
    if status.is_redirection() {
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("(no Location header)");
        return Err(IaError::Http {
            status: status.as_u16(),
            message: format!("redirected to {location}; not followed"),
            retry_after: None,
        });
    }
    if !status.is_success() {
        let retry_after = crate::retry::extract_retry_after(response.headers());
        let body = response.text().await.unwrap_or_default();
        return Err(IaError::Http {
            status: status.as_u16(),
            message: body,
            retry_after,
        });
    }
    Ok(response)
}

/// The text of a truthy `error` field of a JSON object answer: the string
/// itself, or the JSON text of any other truthy value.
fn error_text(answer: &Value) -> Option<String> {
    match answer.get("error")? {
        Value::Null | Value::Bool(false) => None,
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) if n.as_f64() == Some(0.0) => None,
        Value::Array(a) if a.is_empty() => None,
        Value::Object(o) if o.is_empty() => None,
        other => Some(other.to_string()),
    }
}

/// Whether a JSON answer has `"success": false`.
fn is_success_false(answer: &Value) -> bool {
    answer.get("success") == Some(&Value::Bool(false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::IaConfig;
    use crate::error::IaError;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    const COOKIE: &str = "logged-in-user=test%40example.com; logged-in-sig=test-sig";

    fn host_of(server: &MockServer) -> String {
        server.uri().trim_start_matches("http://").to_string()
    }

    /// A config for `host` over plain HTTP with test cookies. S3 keys are set
    /// too, so a request that carried `Authorization: LOW` would show it.
    fn config_for(host: &str) -> IaConfig {
        let mut config = IaConfig::default();
        config.general.host = host.to_string();
        config.general.secure = false;
        config.s3_access = Some("test-access".into());
        config.s3_secret = Some("test-secret".into());
        config
            .cookies
            .insert("logged-in-user".into(), "test%40example.com".into());
        config
            .cookies
            .insert("logged-in-sig".into(), "test-sig".into());
        config
    }

    fn client_for(server: &MockServer) -> IaClient {
        IaClient::from_config(config_for(&host_of(server))).unwrap()
    }

    fn values(email: &str, screenname: &str, itemname: &str) -> Value {
        json!({
            "canonical_email": email,
            "email": email,
            "has_disability_access": false,
            "itemname": itemname,
            "locked": false,
            "notifications": ["example-notice"],
            "screenname": screenname,
            "verified": true,
            "lastlogin": "2026-01-01 00:00:00",
            "createdate": "2020-01-01 00:00:00",
            "field_the_port_does_not_read": {"nested": [1, 2]}
        })
    }

    fn user_1() -> Value {
        values("person@example.com", "example-user-1", "@example-user-1")
    }

    fn ok_body(values: Value) -> Value {
        json!({"success": true, "values": values})
    }

    async fn mount_info(server: &MockServer, response: ResponseTemplate, expect: u64) {
        Mock::given(method("POST"))
            .and(path("/services/xauthn/"))
            .and(query_param("op", "info"))
            .respond_with(response)
            .expect(expect)
            .mount(server)
            .await;
    }

    async fn mount_lock(server: &MockServer, response: ResponseTemplate, expect: u64) {
        Mock::given(method("POST"))
            .and(path("/services/xauthn/"))
            .and(query_param("op", "lock_unlock"))
            .respond_with(response)
            .expect(expect)
            .mount(server)
            .await;
    }

    /// Mounts a catch-all with `expect(0)`: any request the test did not
    /// expect fails it when the server drops.
    async fn mount_nothing_else(server: &MockServer) {
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(599))
            .expect(0)
            .mount(server)
            .await;
    }

    fn decode(s: &str) -> String {
        urlencoding::decode(&s.replace('+', " "))
            .unwrap()
            .into_owned()
    }

    /// The form fields of a request body, decoded, sorted by name.
    fn form_fields(req: &Request) -> Vec<(String, String)> {
        let body = std::str::from_utf8(&req.body).unwrap();
        let mut fields: Vec<(String, String)> = body
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                (decode(k), decode(v))
            })
            .collect();
        fields.sort();
        fields
    }

    fn query_pairs(req: &Request) -> Vec<(String, String)> {
        req.url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    fn header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
        req.headers.get(name).map(|v| v.to_str().unwrap())
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = list
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        v.sort();
        v
    }

    fn assert_account_api(err: &IaError, account: &str, message_part: &str) {
        match err {
            IaError::AccountApi {
                account: got,
                message,
            } => {
                assert_eq!(got, account, "AccountApi should name the input");
                assert!(
                    message.contains(message_part),
                    "message {message:?} should contain {message_part:?}"
                );
            }
            other => panic!("expected AccountApi, got {other:?}"),
        }
    }

    fn assert_auth(err: &IaError, message_part: &str) {
        match err {
            IaError::Auth(message) => assert!(
                message.contains(message_part),
                "message {message:?} should contain {message_part:?}"
            ),
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    // -- lookup type --

    #[test]
    fn detect_leading_at_is_item_name() {
        assert_eq!(LookupType::detect("@example-user"), LookupType::Itemname);
        // A leading `@` wins even when the rest looks like an address.
        assert_eq!(
            LookupType::detect("@person@example.com"),
            LookupType::Itemname
        );
    }

    #[test]
    fn is_email_accepts_addresses() {
        for value in [
            "person@example.com",
            "First.Last+tag@mail.example.org",
            "a@sub.example.org",
        ] {
            assert!(is_email(value), "{value} should be an email address");
            assert_eq!(LookupType::detect(value), LookupType::Email, "{value}");
        }
    }

    #[test]
    fn is_email_rejects_other_values() {
        for value in [
            "example-user-1",
            "person@localhost",
            "person@example.c",
            "a@b..example.com",
            "a@-b.example.com",
            "a@example.com.",
            "a@example.c0m",
            "name with spaces",
        ] {
            assert!(!is_email(value), "{value} should not be an email address");
            assert_eq!(LookupType::detect(value), LookupType::Screenname, "{value}");
        }
    }

    #[test]
    fn lookup_type_field_and_display() {
        assert_eq!(LookupType::Email.field(), "email");
        assert_eq!(LookupType::Screenname.field(), "screenname");
        assert_eq!(LookupType::Itemname.field(), "itemname");
        assert_eq!(LookupType::Email.to_string(), "email");
        assert_eq!(LookupType::Screenname.to_string(), "screenname");
        assert_eq!(LookupType::Itemname.to_string(), "itemname");
    }

    // -- lookup request shape --

    /// Each detected lookup type posts `op=info` with exactly one form field,
    /// the stored cookies, and no `Authorization` header, and keeps the whole
    /// `values` object.
    #[tokio::test]
    async fn lookup_posts_one_field_with_cookies_and_no_authorization() {
        for (user, field, expected_type) in [
            ("person@example.com", "email", LookupType::Email),
            ("example-user-1", "screenname", LookupType::Screenname),
            ("@example-user-1", "itemname", LookupType::Itemname),
        ] {
            let server = MockServer::start().await;
            mount_info(
                &server,
                ResponseTemplate::new(200).set_body_json(ok_body(user_1())),
                1,
            )
            .await;
            let client = client_for(&server);

            let account = lookup(&client, user).await.unwrap();
            assert_eq!(account.lookup, expected_type, "{user}");
            assert_eq!(
                Value::Object(account.values.clone()),
                user_1(),
                "{user}: the whole values object is kept"
            );

            let requests = server.received_requests().await.unwrap();
            assert_eq!(requests.len(), 1, "{user}");
            let req = &requests[0];
            assert_eq!(req.method.as_str(), "POST");
            assert_eq!(req.url.path(), "/services/xauthn/");
            assert_eq!(query_pairs(req), pairs(&[("op", "info")]), "{user}");
            assert_eq!(form_fields(req), pairs(&[(field, user)]), "{user}");
            assert!(
                header(req, "content-type")
                    .unwrap_or_default()
                    .starts_with("application/x-www-form-urlencoded"),
                "{user}: form body"
            );
            assert_eq!(header(req, "cookie"), Some(COOKIE), "{user}");
            assert_eq!(header(req, "authorization"), None, "{user}");
        }
    }

    /// `lookup_by` sends the field it is given, not the detected one.
    #[tokio::test]
    async fn lookup_by_uses_the_given_type() {
        let server = MockServer::start().await;
        let body = ok_body(values(
            "other@example.org",
            "person@example.com",
            "@example-user-1",
        ));
        mount_info(&server, ResponseTemplate::new(200).set_body_json(body), 1).await;
        let client = client_for(&server);

        let account = lookup_by(&client, LookupType::Screenname, "person@example.com")
            .await
            .unwrap();
        assert_eq!(account.lookup, LookupType::Screenname);

        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            form_fields(&requests[0]),
            pairs(&[("screenname", "person@example.com")])
        );
    }

    // -- cookies and host --

    #[tokio::test]
    async fn cookie_values_are_cut_at_the_first_semicolon() {
        let server = MockServer::start().await;
        mount_info(
            &server,
            ResponseTemplate::new(200).set_body_json(ok_body(user_1())),
            1,
        )
        .await;
        let mut config = config_for(&host_of(&server));
        config.cookies.insert(
            "logged-in-user".into(),
            "test%40example.com; path=/; domain=.archive.org".into(),
        );
        config.cookies.insert(
            "logged-in-sig".into(),
            " test-sig ; path=/; domain=.archive.org".into(),
        );
        let client = IaClient::from_config(config).unwrap();

        assert_eq!(cookie_header(&client).unwrap(), COOKIE);
        lookup(&client, "example-user-1").await.unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(header(&requests[0], "cookie"), Some(COOKIE));
    }

    /// A missing or empty cookie fails before any request, for a lookup and
    /// for a lock.
    #[tokio::test]
    async fn missing_or_empty_cookie_is_auth_with_no_request() {
        let cases: [(&str, Option<&str>); 5] = [
            ("logged-in-user", None),
            ("logged-in-sig", None),
            ("logged-in-user", Some("")),
            ("logged-in-sig", Some("")),
            ("logged-in-sig", Some("; path=/")),
        ];
        for (name, value) in cases {
            let server = MockServer::start().await;
            mount_nothing_else(&server).await;
            let mut config = config_for(&host_of(&server));
            match value {
                None => {
                    config.cookies.remove(name);
                }
                Some(v) => {
                    config.cookies.insert(name.into(), v.into());
                }
            }
            let client = IaClient::from_config(config).unwrap();
            let wants = "logged-in-user and logged-in-sig";

            assert_auth(&cookie_header(&client).unwrap_err(), wants);
            assert_auth(&lookup(&client, "example-user-1").await.unwrap_err(), wants);
            assert_auth(
                &set_locked(&client, "@example-user-1", true, Some("note"))
                    .await
                    .unwrap_err(),
                wants,
            );
            // Dropping `server` verifies that no request arrived.
        }
    }

    /// The cookies go only to archive.org, its subdomains and loopback hosts.
    /// Checked on `cookie_header` alone, which sends nothing.
    #[test]
    fn cookie_host_check() {
        // Over https, so only the host decides; the scheme rule has its own
        // test below.
        for host in [
            "archive.org",
            "www.archive.org",
            "ia800100.us.archive.org",
            "localhost",
            "localhost:8080",
            "127.0.0.1",
            "127.0.0.1:8080",
            "[::1]:8080",
        ] {
            let client = IaClient::from_config(https_config_for(host)).unwrap();
            assert_eq!(cookie_header(&client).unwrap(), COOKIE, "{host}");
        }
        for host in [
            "example.org",
            "example.org:8080",
            "notarchive.org",
            "archive.org.example.org",
            "0.0.0.0:8080",
        ] {
            let client = IaClient::from_config(https_config_for(host)).unwrap();
            assert_auth(
                &cookie_header(&client).unwrap_err(),
                "sends login cookies only to archive.org",
            );
        }
    }

    /// A config for `host` over https with test cookies.
    fn https_config_for(host: &str) -> IaConfig {
        let mut config = config_for(host);
        config.general.secure = true;
        config
    }

    /// Plain http carries the cookies only to loopback hosts.
    #[test]
    fn cookie_header_needs_https_except_on_loopback() {
        for host in ["archive.org", "www.archive.org"] {
            let client = IaClient::from_config(config_for(host)).unwrap();
            let err = cookie_header(&client).unwrap_err();
            assert_auth(&err, "sends login cookies only to archive.org over https");
            assert_auth(&err, "uses http");
        }
        for host in [
            "127.0.0.1:8080",
            "localhost:8080",
            "[::1]:8080",
            "127.0.0.1",
        ] {
            let client = IaClient::from_config(config_for(host)).unwrap();
            assert_eq!(cookie_header(&client).unwrap(), COOKIE, "{host}");
        }
    }

    /// A host string with a user name or password would make reqwest send
    /// `Authorization: Basic`; it is refused, without echoing the password.
    #[test]
    fn cookie_header_refuses_user_information() {
        for host in [
            "user:example-password@archive.org",
            "user@archive.org",
            ":example-password@archive.org",
            "user:example-password@127.0.0.1:8080",
        ] {
            let client = IaClient::from_config(https_config_for(host)).unwrap();
            let err = cookie_header(&client).unwrap_err();
            assert_auth(&err, "the host names a user or password");
            assert!(
                !err.to_string().contains("example-password"),
                "{host}: the password must not be echoed: {err}"
            );
        }
    }

    /// Host strings that only look like archive.org are judged by the host
    /// the URL really names. Letter case does not matter, and a trailing
    /// dot (a fully qualified name for the same host) is allowed.
    #[test]
    fn cookie_header_judges_the_parsed_host() {
        for host in [
            "archive.org@example.org",
            "example.org#.archive.org",
            "example.org\\.archive.org",
            "example.org/.archive.org",
            "example.org?.archive.org",
        ] {
            let client = IaClient::from_config(https_config_for(host)).unwrap();
            assert!(cookie_header(&client).is_err(), "{host} must be refused");
        }
        for host in [
            "ARCHIVE.ORG",
            "Www.Archive.Org",
            "archive.org.",
            "www.archive.org.",
        ] {
            let client = IaClient::from_config(https_config_for(host)).unwrap();
            assert_eq!(cookie_header(&client).unwrap(), COOKIE, "{host}");
        }
    }

    /// A host outside the allowed set fails before any request. `0.0.0.0`
    /// is outside the set and, if a request were wrongly sent, can only
    /// reach this machine, where the mock counts it.
    #[tokio::test]
    async fn non_archive_host_is_auth_with_no_request() {
        let server = MockServer::start().await;
        mount_nothing_else(&server).await;
        let port = server.address().port();
        let client = IaClient::from_config(config_for(&format!("0.0.0.0:{port}"))).unwrap();

        let wants = "sends login cookies only to archive.org";
        assert_auth(&lookup(&client, "example-user-1").await.unwrap_err(), wants);
        assert_auth(
            &set_locked(&client, "@example-user-1", true, None)
                .await
                .unwrap_err(),
            wants,
        );
    }

    #[tokio::test]
    async fn localhost_and_127_0_0_1_with_a_port_are_allowed() {
        let server = MockServer::start().await;
        mount_info(
            &server,
            ResponseTemplate::new(200).set_body_json(ok_body(user_1())),
            2,
        )
        .await;
        let port = server.address().port();
        for host in [format!("localhost:{port}"), format!("127.0.0.1:{port}")] {
            let client = IaClient::from_config(config_for(&host)).unwrap();
            let account = lookup(&client, "example-user-1").await.unwrap();
            assert_eq!(account.screenname(), Some("example-user-1"), "{host}");
        }
    }

    // -- lookup errors --

    #[tokio::test]
    async fn lookup_error_bodies_are_account_api() {
        let cases: [(Value, &str); 6] = [
            (
                json!({"success": false, "error": "account not found", "values": {}}),
                "account not found",
            ),
            (json!({"success": true, "values": {}}), "account not found"),
            (
                json!({"success": false, "values": user_1()}),
                "account not found",
            ),
            (json!({"success": true}), "account not found"),
            (json!({"success": true, "values": []}), "account not found"),
            (
                json!({"success": true, "error": "example failure", "values": user_1()}),
                "example failure",
            ),
        ];
        for (body, message_part) in cases {
            let server = MockServer::start().await;
            mount_info(&server, ResponseTemplate::new(200).set_body_json(&body), 1).await;
            let client = client_for(&server);
            let err = lookup(&client, "nobody@example.org").await.unwrap_err();
            assert_account_api(&err, "nobody@example.org", message_part);
        }
    }

    #[tokio::test]
    async fn lookup_non_json_answer_is_account_api() {
        let server = MockServer::start().await;
        mount_info(
            &server,
            ResponseTemplate::new(200).set_body_string("<html>not json</html>"),
            1,
        )
        .await;
        let client = client_for(&server);
        let err = lookup(&client, "example-user-1").await.unwrap_err();
        assert_account_api(&err, "example-user-1", "response is not JSON");
        assert_account_api(&err, "example-user-1", "<html>not json</html>");
    }

    #[tokio::test]
    async fn lookup_400_html_is_http_with_the_body() {
        let server = MockServer::start().await;
        let html = "<html><body>Bad Request</body></html>";
        mount_info(&server, ResponseTemplate::new(400).set_body_string(html), 1).await;
        let client = client_for(&server);
        match lookup(&client, "example-user-1").await.unwrap_err() {
            IaError::Http {
                status, message, ..
            } => {
                assert_eq!(status, 400);
                assert_eq!(message, html);
            }
            other => panic!("expected Http 400, got {other:?}"),
        }
    }

    /// A redirect is reported, not followed: the target is never requested.
    #[tokio::test]
    async fn lookup_redirect_is_http_and_not_followed() {
        let server = MockServer::start().await;
        let target = format!("{}/elsewhere/", server.uri());
        mount_info(
            &server,
            ResponseTemplate::new(302).insert_header("Location", target.as_str()),
            1,
        )
        .await;
        Mock::given(path("/elsewhere/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body(user_1())))
            .expect(0)
            .mount(&server)
            .await;
        let client = client_for(&server);
        match lookup(&client, "example-user-1").await.unwrap_err() {
            IaError::Http {
                status, message, ..
            } => {
                assert_eq!(status, 302);
                assert!(
                    message.contains(&format!("redirected to {target}")),
                    "message: {message}"
                );
                assert!(message.contains("not followed"), "message: {message}");
            }
            other => panic!("expected Http 302, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn lookup_429_is_rate_limited() {
        for (response, wants) in [
            (
                ResponseTemplate::new(429).insert_header("Retry-After", "5"),
                5,
            ),
            (ResponseTemplate::new(429), 30),
        ] {
            let server = MockServer::start().await;
            mount_info(&server, response, 1).await;
            let client = client_for(&server);
            match lookup(&client, "example-user-1").await.unwrap_err() {
                IaError::RateLimited { retry_after } => assert_eq!(retry_after, wants),
                other => panic!("expected RateLimited, got {other:?}"),
            }
        }
    }

    /// A `Retry-After` of zero or a past date still waits one second, so a
    /// caller that resends on every 429 never resends with no wait.
    #[tokio::test]
    async fn retry_after_zero_or_past_waits_one_second() {
        for value in ["0", "Thu, 01 Jan 1970 00:00:00 GMT"] {
            let server = MockServer::start().await;
            mount_info(
                &server,
                ResponseTemplate::new(429).insert_header("Retry-After", value),
                1,
            )
            .await;
            mount_lock(
                &server,
                ResponseTemplate::new(429).insert_header("Retry-After", value),
                1,
            )
            .await;
            let client = client_for(&server);
            match lookup(&client, "example-user-1").await.unwrap_err() {
                IaError::RateLimited { retry_after } => assert_eq!(retry_after, 1, "{value}"),
                other => panic!("{value}: expected RateLimited, got {other:?}"),
            }
            match set_locked(&client, "@example-user-1", true, None)
                .await
                .unwrap_err()
            {
                IaError::RateLimited { retry_after } => assert_eq!(retry_after, 1, "{value}"),
                other => panic!("{value}: expected RateLimited, got {other:?}"),
            }
        }
    }

    /// A 5xx fails the lookup after one request: no retry layer.
    #[tokio::test]
    async fn lookup_500_is_http_after_one_request() {
        let server = MockServer::start().await;
        mount_info(&server, ResponseTemplate::new(500), 1).await;
        let client = client_for(&server);
        match lookup(&client, "example-user-1").await.unwrap_err() {
            IaError::Http { status, .. } => assert_eq!(status, 500),
            other => panic!("expected Http 500, got {other:?}"),
        }
    }

    // -- identity check --

    #[tokio::test]
    async fn lookup_answered_with_a_different_account_is_account_api() {
        let other = values("other@example.org", "example-user-2", "@example-user-2");
        let mut no_screenname = user_1();
        no_screenname.as_object_mut().unwrap().remove("screenname");
        for (user, answer) in [
            ("person@example.com", other.clone()),
            ("example-user-1", other.clone()),
            ("@example-user-1", other.clone()),
            ("example-user-1", no_screenname),
        ] {
            let server = MockServer::start().await;
            mount_info(
                &server,
                ResponseTemplate::new(200).set_body_json(ok_body(answer)),
                1,
            )
            .await;
            let client = client_for(&server);
            let err = lookup(&client, user).await.unwrap_err();
            assert_account_api(&err, user, "lookup answered with a different account");
        }
    }

    #[tokio::test]
    async fn identity_check_ignores_letter_case() {
        for user in ["Person@Example.COM", "Example-User-1", "@EXAMPLE-USER-1"] {
            let server = MockServer::start().await;
            mount_info(
                &server,
                ResponseTemplate::new(200).set_body_json(ok_body(user_1())),
                1,
            )
            .await;
            let client = client_for(&server);
            let account = lookup(&client, user).await.unwrap();
            assert_eq!(account.itemname(), Some("@example-user-1"), "{user}");
        }
    }

    #[tokio::test]
    async fn email_lookup_matching_only_canonical_email_passes() {
        let server = MockServer::start().await;
        let mut answer = user_1();
        answer["canonical_email"] = json!("alias@example.org");
        mount_info(
            &server,
            ResponseTemplate::new(200).set_body_json(ok_body(answer)),
            1,
        )
        .await;
        let client = client_for(&server);
        let account = lookup(&client, "alias@example.org").await.unwrap();
        assert_eq!(account.lookup, LookupType::Email);
        assert_eq!(account.email(), Some("person@example.com"));
        assert_eq!(account.canonical_email(), Some("alias@example.org"));
    }

    // -- accessors --

    #[test]
    fn accessors_read_their_fields() {
        let account = Account {
            lookup: LookupType::Screenname,
            values: user_1().as_object().unwrap().clone(),
        };
        assert_eq!(account.email(), Some("person@example.com"));
        assert_eq!(account.canonical_email(), Some("person@example.com"));
        assert_eq!(account.screenname(), Some("example-user-1"));
        assert_eq!(account.itemname(), Some("@example-user-1"));
        assert_eq!(account.locked(), Some(false));
    }

    #[test]
    fn accessors_tolerate_null_and_wrong_types() {
        let mut v = user_1();
        v["canonical_email"] = Value::Null;
        v["locked"] = json!("yes");
        let account = Account {
            lookup: LookupType::Screenname,
            values: v.as_object().unwrap().clone(),
        };
        assert_eq!(account.canonical_email(), None);
        assert_eq!(account.itemname(), Some("@example-user-1"));
        assert_eq!(account.locked(), None);
    }

    // -- set_locked --

    #[tokio::test]
    async fn set_locked_posts_lock_unlock_form() {
        for (locked, comment, expected) in [
            (
                true,
                Some("example lock note"),
                pairs(&[
                    ("itemname", "@example-user-1"),
                    ("is_lock", "1"),
                    ("comments", "example lock note"),
                ]),
            ),
            (
                false,
                Some("example unlock note"),
                pairs(&[
                    ("itemname", "@example-user-1"),
                    ("is_lock", "0"),
                    ("comments", "example unlock note"),
                ]),
            ),
            (
                true,
                None,
                pairs(&[("itemname", "@example-user-1"), ("is_lock", "1")]),
            ),
            (
                false,
                Some(""),
                pairs(&[("itemname", "@example-user-1"), ("is_lock", "0")]),
            ),
        ] {
            let server = MockServer::start().await;
            mount_lock(
                &server,
                ResponseTemplate::new(200).set_body_json(json!({"success": true})),
                1,
            )
            .await;
            let client = client_for(&server);
            set_locked(&client, "@example-user-1", locked, comment)
                .await
                .unwrap();

            let requests = server.received_requests().await.unwrap();
            assert_eq!(requests.len(), 1);
            let req = &requests[0];
            assert_eq!(req.method.as_str(), "POST");
            assert_eq!(req.url.path(), "/services/xauthn/");
            assert_eq!(query_pairs(req), pairs(&[("op", "lock_unlock")]));
            assert_eq!(
                form_fields(req),
                expected,
                "locked={locked} comment={comment:?}"
            );
            assert_eq!(header(req, "cookie"), Some(COOKIE));
            assert_eq!(header(req, "authorization"), None);
        }
    }

    #[tokio::test]
    async fn lock_and_unlock_wrappers_send_is_lock() {
        let server = MockServer::start().await;
        mount_lock(
            &server,
            ResponseTemplate::new(200).set_body_json(json!({"success": true})),
            2,
        )
        .await;
        let client = client_for(&server);
        lock(&client, "@example-user-1", Some("note"))
            .await
            .unwrap();
        unlock(&client, "@example-user-1", None).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            form_fields(&requests[0]),
            pairs(&[
                ("itemname", "@example-user-1"),
                ("is_lock", "1"),
                ("comments", "note")
            ])
        );
        assert_eq!(
            form_fields(&requests[1]),
            pairs(&[("itemname", "@example-user-1"), ("is_lock", "0")])
        );
    }

    #[tokio::test]
    async fn set_locked_returns_json_or_text_answer() {
        let server = MockServer::start().await;
        let answer = json!({"success": true, "values": {"note": "example"}});
        mount_lock(
            &server,
            ResponseTemplate::new(200).set_body_json(&answer),
            1,
        )
        .await;
        let client = client_for(&server);
        let got = set_locked(&client, "@example-user-1", true, None)
            .await
            .unwrap();
        assert_eq!(got, answer);

        let server = MockServer::start().await;
        mount_lock(
            &server,
            ResponseTemplate::new(200).set_body_string("account locked"),
            1,
        )
        .await;
        let client = client_for(&server);
        let got = set_locked(&client, "@example-user-1", true, None)
            .await
            .unwrap();
        assert_eq!(got, Value::String("account locked".into()));
    }

    #[tokio::test]
    async fn set_locked_refusal_is_account_api() {
        for (locked, body, message_part) in [
            (
                true,
                json!({"success": false, "error": "example refusal"}),
                "example refusal",
            ),
            (true, json!({"error": "example refusal"}), "example refusal"),
            (true, json!({"success": false}), "lock refused"),
            (false, json!({"success": false}), "unlock refused"),
        ] {
            let server = MockServer::start().await;
            mount_lock(&server, ResponseTemplate::new(200).set_body_json(&body), 1).await;
            let client = client_for(&server);
            let err = set_locked(&client, "@example-user-1", locked, None)
                .await
                .unwrap_err();
            assert_account_api(&err, "@example-user-1", message_part);
        }
    }

    /// A 5xx on a lock is never replayed: the server may already have acted.
    #[tokio::test]
    async fn set_locked_500_is_http_after_one_request() {
        let server = MockServer::start().await;
        mount_lock(&server, ResponseTemplate::new(500), 1).await;
        let client = client_for(&server);
        match set_locked(&client, "@example-user-1", true, Some("note"))
            .await
            .unwrap_err()
        {
            IaError::Http { status, .. } => assert_eq!(status, 500),
            other => panic!("expected Http 500, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_locked_429_and_redirect() {
        let server = MockServer::start().await;
        mount_lock(
            &server,
            ResponseTemplate::new(429).insert_header("Retry-After", "5"),
            1,
        )
        .await;
        let client = client_for(&server);
        match set_locked(&client, "@example-user-1", true, None)
            .await
            .unwrap_err()
        {
            IaError::RateLimited { retry_after } => assert_eq!(retry_after, 5),
            other => panic!("expected RateLimited, got {other:?}"),
        }

        let server = MockServer::start().await;
        let target = format!("{}/elsewhere/", server.uri());
        mount_lock(
            &server,
            ResponseTemplate::new(302).insert_header("Location", target.as_str()),
            1,
        )
        .await;
        Mock::given(path("/elsewhere/"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let client = client_for(&server);
        match set_locked(&client, "@example-user-1", true, None)
            .await
            .unwrap_err()
        {
            IaError::Http { status, .. } => assert_eq!(status, 302),
            other => panic!("expected Http 302, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_locked_unusable_itemname_is_account_api_with_no_request() {
        for itemname in ["example-user-1", "@", ""] {
            let server = MockServer::start().await;
            mount_nothing_else(&server).await;
            let client = client_for(&server);
            let err = set_locked(&client, itemname, true, Some("note"))
                .await
                .unwrap_err();
            assert_account_api(&err, itemname, "lookup returned no usable itemname");
        }
    }

    // -- the error variant --

    #[test]
    fn account_api_error_json_and_retry() {
        let err = IaError::AccountApi {
            account: "nobody@example.org".into(),
            message: "account not found".into(),
        };
        assert_eq!(
            err.to_string(),
            "account API error for nobody@example.org: account not found"
        );
        assert!(!err.is_retryable());
        let json = serde_json::to_value(err.to_json_error()).unwrap();
        assert_eq!(json["error"]["code"], "account_api_error");
        assert_eq!(json["error"]["account"], "nobody@example.org");
        assert_eq!(
            json["error"]["message"],
            "account API error for nobody@example.org: account not found"
        );
    }
}
