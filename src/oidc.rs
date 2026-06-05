use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::LazyLock;
use std::time::Instant;

use cot::db::Database;
use cot::request::extractors::UrlQuery;
use cot::session::Session;
use openidconnect::core::{CoreClient, CoreProviderMetadata};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet, EndpointNotSet,
    EndpointSet, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
};

use cot::request::RequestHead;
use cot::request::extractors::FromRequestHead;

use crate::auth;
use crate::config::AppConfig;
use crate::i18n::I18n;
use crate::user::{OidcLink, User};

// ---------------------------------------------------------------------------
// Request origin extractor (scheme + host from headers)
// ---------------------------------------------------------------------------

/// Extracts the origin (e.g. "http://127.0.0.1:3001") from the request so we
/// can build the correct OIDC redirect URI.
pub struct RequestOrigin(pub String);

impl FromRequestHead for RequestOrigin {
    async fn from_request_head(head: &RequestHead) -> cot::Result<Self> {
        let scheme = head
            .headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("http");

        let host = head
            .headers
            .get(cot::http::header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("localhost");

        Ok(RequestOrigin(format!("{scheme}://{host}")))
    }
}

// ---------------------------------------------------------------------------
// Session keys for OIDC flow state
// ---------------------------------------------------------------------------

const SESSION_CSRF_STATE: &str = "oidc_csrf_state";
const SESSION_NONCE: &str = "oidc_nonce";
const SESSION_PKCE_VERIFIER: &str = "oidc_pkce_verifier";
const SESSION_REDIRECT_URI: &str = "oidc_redirect_uri";

const SESSION_MOBILE_CSRF_STATE: &str = "mobile_oidc_csrf_state";
const SESSION_MOBILE_NONCE: &str = "mobile_oidc_nonce";
const SESSION_MOBILE_PKCE_VERIFIER: &str = "mobile_oidc_pkce_verifier";
const SESSION_MOBILE_PROVIDER_REDIRECT_URI: &str = "mobile_oidc_provider_redirect_uri";
const SESSION_MOBILE_APP_REDIRECT_URI: &str = "mobile_oidc_app_redirect_uri";
const DEFAULT_MOBILE_REDIRECT_URI: &str = "furumi://auth/callback";

// ---------------------------------------------------------------------------
// Provider cache
// ---------------------------------------------------------------------------

/// Concrete client type returned by `from_provider_metadata` + `set_redirect_uri`.
/// The provider metadata discovery sets auth URL to EndpointSet, and token/userinfo
/// endpoints to EndpointMaybeSet. The remaining endpoints stay EndpointNotSet.
type ConfiguredClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

struct CachedProvider {
    client: ConfiguredClient,
    fetched_at: Instant,
    config_hash: u64,
}

static PROVIDER_CACHE: LazyLock<tokio::sync::RwLock<Option<CachedProvider>>> =
    LazyLock::new(|| tokio::sync::RwLock::new(None));

/// TTL for cached provider metadata (1 hour).
const PROVIDER_TTL_SECS: u64 = 3600;

/// Compute a hash of the OIDC configuration values so we can detect changes.
fn config_hash(issuer: &str, client_id: &str, client_secret: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    issuer.hash(&mut hasher);
    client_id.hash(&mut hasher);
    client_secret.hash(&mut hasher);
    hasher.finish()
}

fn oidc_http_client() -> reqwest::Client {
    reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("valid reqwest client")
}

/// Get or refresh the cached OIDC provider. Returns a cloned `ConfiguredClient`.
async fn get_or_refresh_provider(
    config: &AppConfig,
    http: &reqwest::Client,
) -> Result<ConfiguredClient, String> {
    let hash = config_hash(
        &config.oidc_issuer,
        &config.oidc_client_id,
        &config.oidc_client_secret,
    );

    // Fast path: check if we have a valid cached provider.
    {
        let cache = PROVIDER_CACHE.read().await;
        if let Some(ref cached) = *cache {
            if cached.config_hash == hash
                && cached.fetched_at.elapsed().as_secs() < PROVIDER_TTL_SECS
            {
                return Ok(cached.client.clone());
            }
        }
    }

    // Slow path: discover provider metadata + JWKS.
    // Strip /.well-known/openid-configuration suffix if the user pasted the
    // full discovery URL, so discover_async doesn't double-append it.
    let issuer = config
        .oidc_issuer
        .trim_end_matches('/')
        .strip_suffix("/.well-known/openid-configuration")
        .unwrap_or(config.oidc_issuer.trim_end_matches('/'))
        .to_owned();

    let issuer_url = IssuerUrl::new(issuer).map_err(|e| format!("invalid issuer URL: {e}"))?;

    let metadata = CoreProviderMetadata::discover_async(issuer_url, http)
        .await
        .map_err(|e| format!("OIDC discovery failed: {e}"))?;

    let client = CoreClient::from_provider_metadata(
        metadata,
        ClientId::new(config.oidc_client_id.clone()),
        Some(ClientSecret::new(config.oidc_client_secret.clone())),
    );

    let mut cache = PROVIDER_CACHE.write().await;
    *cache = Some(CachedProvider {
        client: client.clone(),
        fetched_at: Instant::now(),
        config_hash: hash,
    });

    Ok(client)
}

// ---------------------------------------------------------------------------
// GET /auth/oidc/start
// ---------------------------------------------------------------------------

pub async fn oidc_start_handler(
    origin: RequestOrigin,
    i18n: I18n,
    db: Database,
    session: Session,
) -> cot::Result<cot::response::Response> {
    let (config, _) = AppConfig::load_with_db(&db).await;

    // Validate SSO is enabled and configured.
    if !config.auth_sso_enabled
        || config.oidc_issuer.is_empty()
        || config.oidc_client_id.is_empty()
        || config.oidc_client_secret.is_empty()
    {
        tracing::warn!("OIDC start requested but SSO is not configured");
        crate::metrics::record_auth_attempt("oidc", "failure", "not_configured");
        return redirect_login_with_error(i18n.t.login_sso_disabled);
    }

    let http = oidc_http_client();
    let client = match get_or_refresh_provider(&config, &http).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("OIDC provider error: {e}");
            crate::metrics::record_auth_attempt("oidc", "failure", "provider_error");
            return redirect_login_with_error(i18n.t.login_oidc_error);
        }
    };

    // Build redirect URI from the actual request origin.
    let redirect_uri_str = format!("{}/auth/oidc/callback", origin.0);
    let redirect_url = RedirectUrl::new(redirect_uri_str.clone())
        .map_err(|e| cot::Error::internal(format!("bad redirect URI: {e}")))?;
    let client = client.set_redirect_uri(redirect_url);
    tracing::info!(
        redirect_uri = %redirect_uri_str,
        oidc_issuer = %config.oidc_issuer,
        "OIDC start: building authorization request",
    );

    // Build PKCE challenge.
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

    // Build authorization URL.
    // The openid scope is added automatically by the crate; only add email + profile.
    let (auth_url, csrf_state, nonce) = client
        .authorize_url(
            openidconnect::AuthenticationFlow::<openidconnect::core::CoreResponseType>::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .add_scope(Scope::new("email".to_string()))
        .add_scope(Scope::new("profile".to_string()))
        .set_pkce_challenge(pkce_challenge)
        .url();
    tracing::info!(auth_url = %auth_url, "OIDC start: redirecting to provider");

    // Store OIDC flow state in the session.
    session
        .insert(SESSION_CSRF_STATE, csrf_state.secret().clone())
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    session
        .insert(SESSION_NONCE, nonce.secret().clone())
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    session
        .insert(SESSION_PKCE_VERIFIER, pkce_verifier.secret().clone())
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    session
        .insert(SESSION_REDIRECT_URI, redirect_uri_str)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;

    Ok(auth::redirect(auth_url.as_str()))
}

// ---------------------------------------------------------------------------
// GET /auth/oidc/callback
// ---------------------------------------------------------------------------

use serde::Deserialize;

#[derive(Deserialize)]
pub struct OidcCallbackQuery {
    code: String,
    state: String,
}

#[derive(Deserialize)]
pub struct MobileOidcStartQuery {
    redirect_uri: Option<String>,
}

#[derive(Deserialize)]
pub struct MobileOidcCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

pub async fn oidc_callback_handler(
    i18n: I18n,
    db: Database,
    session: Session,
    UrlQuery(query): UrlQuery<OidcCallbackQuery>,
) -> cot::Result<cot::response::Response> {
    let (config, _) = AppConfig::load_with_db(&db).await;

    // Retrieve OIDC flow state from the session.
    let saved_csrf: Option<String> = session
        .get(SESSION_CSRF_STATE)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    let saved_nonce: Option<String> = session
        .get(SESSION_NONCE)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    let saved_pkce: Option<String> = session
        .get(SESSION_PKCE_VERIFIER)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    let saved_redirect_uri: Option<String> = session
        .get(SESSION_REDIRECT_URI)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;

    // Validate CSRF state.
    let Some(saved_csrf) = saved_csrf else {
        tracing::warn!("OIDC callback: no CSRF state in session");
        crate::metrics::record_auth_attempt("oidc", "failure", "missing_state");
        return redirect_login_with_error(i18n.t.login_oidc_error);
    };
    if query.state != saved_csrf {
        tracing::warn!("OIDC callback: CSRF state mismatch");
        crate::metrics::record_auth_attempt("oidc", "failure", "csrf");
        return redirect_login_with_error(i18n.t.login_oidc_error);
    }

    let Some(nonce_str) = saved_nonce else {
        tracing::warn!("OIDC callback: no nonce in session");
        crate::metrics::record_auth_attempt("oidc", "failure", "missing_nonce");
        return redirect_login_with_error(i18n.t.login_oidc_error);
    };
    let Some(pkce_str) = saved_pkce else {
        tracing::warn!("OIDC callback: no PKCE verifier in session");
        crate::metrics::record_auth_attempt("oidc", "failure", "missing_pkce");
        return redirect_login_with_error(i18n.t.login_oidc_error);
    };

    let nonce = Nonce::new(nonce_str);
    let pkce_verifier = PkceCodeVerifier::new(pkce_str);

    let http = oidc_http_client();
    let client = match get_or_refresh_provider(&config, &http).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("OIDC provider error during callback: {e}");
            crate::metrics::record_auth_attempt("oidc", "failure", "provider_error");
            return redirect_login_with_error(i18n.t.login_oidc_error);
        }
    };

    // Restore the redirect URI that was used in the authorization request.
    let client = if let Some(ref uri) = saved_redirect_uri {
        let redirect_url = RedirectUrl::new(uri.clone())
            .map_err(|e| cot::Error::internal(format!("bad redirect URI from session: {e}")))?;
        client.set_redirect_uri(redirect_url)
    } else {
        client
    };

    // Exchange code for tokens.
    let token_request = match client.exchange_code(AuthorizationCode::new(query.code.clone())) {
        Ok(req) => req,
        Err(e) => {
            tracing::error!("OIDC token endpoint not configured: {e}");
            crate::metrics::record_auth_attempt("oidc", "failure", "token_config");
            return redirect_login_with_error(i18n.t.login_oidc_error);
        }
    };
    let token_response = token_request
        .set_pkce_verifier(pkce_verifier)
        .request_async(&http)
        .await;

    let token_response = match token_response {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("OIDC token exchange failed: {e}");
            crate::metrics::record_auth_attempt("oidc", "failure", "token_exchange");
            return redirect_login_with_error(i18n.t.login_oidc_error);
        }
    };

    // Verify and extract ID token claims.
    use openidconnect::TokenResponse;
    let id_token = match token_response.id_token() {
        Some(t) => t,
        None => {
            tracing::error!("OIDC response missing ID token");
            crate::metrics::record_auth_attempt("oidc", "failure", "missing_id_token");
            return redirect_login_with_error(i18n.t.login_oidc_error);
        }
    };

    let claims = match id_token.claims(&client.id_token_verifier(), &nonce) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("OIDC ID token verification failed: {e}");
            crate::metrics::record_auth_attempt("oidc", "failure", "id_token_verify");
            return redirect_login_with_error(i18n.t.login_oidc_error);
        }
    };

    let sub = claims.subject().to_string();
    let issuer = claims.issuer().to_string();
    let email = claims.email().map(|e| e.to_string());
    let name = claims
        .name()
        .and_then(|n| n.get(None))
        .map(|n| n.to_string());

    // Extract groups from the raw JWT payload (second dot-separated segment).
    // The token is already signature-verified above, so we only need to decode
    // the payload to read the non-standard `groups` claim.
    let groups: Vec<String> = (|| {
        use base64::Engine;
        let raw = id_token.to_string();
        let payload_b64 = raw.split('.').nth(1)?;
        // JWT payloads use URL-safe base64; try without padding first, then
        // fall back to the padded variant (some providers add trailing '=').
        let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload_b64)
            .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload_b64))
            .ok()?;
        let value: serde_json::Value = serde_json::from_slice(&payload_bytes).ok()?;
        let arr = value.get("groups")?.as_array()?;
        Some(
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
        )
    })()
    .unwrap_or_default();

    tracing::info!(
        "OIDC login: sub={sub}, groups={groups:?}, admin_groups={:?}, user_groups={:?}",
        config.oidc_admin_groups,
        config.oidc_user_groups,
    );

    if !is_allowed_by_groups(&groups, &config.oidc_user_groups, &config.oidc_admin_groups) {
        tracing::warn!(
            "OIDC login denied by group allowlist: sub={sub}, groups={groups:?}, user_groups={:?}, admin_groups={:?}",
            config.oidc_user_groups,
            config.oidc_admin_groups,
        );
        crate::metrics::record_auth_attempt("oidc", "failure", "not_in_group");
        return redirect_login_with_error(i18n.t.login_access_denied);
    }

    // User provisioning logic.
    let user = match provision_user(
        &db,
        &issuer,
        &sub,
        email.as_deref(),
        name.as_deref(),
        &groups,
        &config.oidc_admin_groups,
    )
    .await
    {
        Ok(u) => u,
        Err(e) => {
            tracing::error!("OIDC user provisioning failed: {e}");
            crate::metrics::record_auth_attempt("oidc", "failure", "provisioning");
            return redirect_login_with_error(i18n.t.login_oidc_error);
        }
    };

    let redirect_to = auth::get_post_login_redirect(&session)
        .await?
        .unwrap_or_else(|| "/".to_string());

    // Log the user in.
    auth::login(&session, user.id_val()).await?;
    auth::clear_post_login_redirect(&session).await?;
    crate::metrics::record_auth_attempt("oidc", "success", "ok");
    crate::metrics::record_session_created("oidc");

    // Clear OIDC session keys.
    let _: Option<String> = session
        .remove(SESSION_CSRF_STATE)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    let _: Option<String> = session
        .remove(SESSION_NONCE)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    let _: Option<String> = session
        .remove(SESSION_PKCE_VERIFIER)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    let _: Option<String> = session
        .remove(SESSION_REDIRECT_URI)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;

    Ok(auth::redirect(&redirect_to))
}

// ---------------------------------------------------------------------------
// Mobile OIDC flow
// ---------------------------------------------------------------------------

pub async fn oidc_mobile_start_handler(
    origin: RequestOrigin,
    db: Database,
    session: Session,
    UrlQuery(query): UrlQuery<MobileOidcStartQuery>,
) -> cot::Result<cot::response::Response> {
    let Some(app_redirect_uri) = safe_mobile_redirect_uri(query.redirect_uri.as_deref()) else {
        crate::metrics::record_auth_attempt("mobile_oidc", "failure", "bad_redirect_uri");
        return Ok(text_response(
            cot::http::StatusCode::BAD_REQUEST,
            "invalid mobile redirect_uri",
        ));
    };

    let (config, _) = AppConfig::load_with_db(&db).await;

    if !config.auth_sso_enabled
        || config.oidc_issuer.is_empty()
        || config.oidc_client_id.is_empty()
        || config.oidc_client_secret.is_empty()
    {
        tracing::warn!("Mobile OIDC start requested but SSO is not configured");
        crate::metrics::record_auth_attempt("mobile_oidc", "failure", "not_configured");
        return Ok(mobile_redirect_error(
            &app_redirect_uri,
            "sso_not_configured",
        ));
    }

    let http = oidc_http_client();
    let client = match get_or_refresh_provider(&config, &http).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Mobile OIDC provider error: {e}");
            crate::metrics::record_auth_attempt("mobile_oidc", "failure", "provider_error");
            return Ok(mobile_redirect_error(&app_redirect_uri, "provider_error"));
        }
    };

    let provider_redirect_uri = format!("{}/auth/mobile/oidc/callback", origin.0);
    let redirect_url = RedirectUrl::new(provider_redirect_uri.clone())
        .map_err(|e| cot::Error::internal(format!("bad mobile redirect URI: {e}")))?;
    let client = client.set_redirect_uri(redirect_url);

    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let (auth_url, csrf_state, nonce) = client
        .authorize_url(
            openidconnect::AuthenticationFlow::<openidconnect::core::CoreResponseType>::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .add_scope(Scope::new("email".to_string()))
        .add_scope(Scope::new("profile".to_string()))
        .set_pkce_challenge(pkce_challenge)
        .url();

    session
        .insert(SESSION_MOBILE_CSRF_STATE, csrf_state.secret().clone())
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    session
        .insert(SESSION_MOBILE_NONCE, nonce.secret().clone())
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    session
        .insert(SESSION_MOBILE_PKCE_VERIFIER, pkce_verifier.secret().clone())
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    session
        .insert(
            SESSION_MOBILE_PROVIDER_REDIRECT_URI,
            provider_redirect_uri.clone(),
        )
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    session
        .insert(SESSION_MOBILE_APP_REDIRECT_URI, app_redirect_uri)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;

    tracing::info!(
        auth_url = %auth_url,
        provider_redirect_uri = %provider_redirect_uri,
        "Mobile OIDC start: redirecting to provider",
    );

    Ok(auth::redirect(auth_url.as_str()))
}

pub async fn oidc_mobile_callback_handler(
    db: Database,
    session: Session,
    UrlQuery(query): UrlQuery<MobileOidcCallbackQuery>,
) -> cot::Result<cot::response::Response> {
    let app_redirect_uri = mobile_app_redirect_uri_from_session(&session).await?;

    if query.error.is_some() {
        tracing::warn!("Mobile OIDC callback returned provider error");
        crate::metrics::record_auth_attempt("mobile_oidc", "failure", "provider_denied");
        clear_mobile_oidc_session(&session).await?;
        return Ok(mobile_redirect_error(&app_redirect_uri, "provider_denied"));
    }

    let Some(code) = query.code else {
        crate::metrics::record_auth_attempt("mobile_oidc", "failure", "missing_code");
        clear_mobile_oidc_session(&session).await?;
        return Ok(mobile_redirect_error(&app_redirect_uri, "missing_code"));
    };
    let Some(state) = query.state else {
        crate::metrics::record_auth_attempt("mobile_oidc", "failure", "missing_state");
        clear_mobile_oidc_session(&session).await?;
        return Ok(mobile_redirect_error(&app_redirect_uri, "missing_state"));
    };

    let saved_csrf: Option<String> = session
        .get(SESSION_MOBILE_CSRF_STATE)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    let saved_nonce: Option<String> = session
        .get(SESSION_MOBILE_NONCE)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    let saved_pkce: Option<String> = session
        .get(SESSION_MOBILE_PKCE_VERIFIER)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    let provider_redirect_uri: Option<String> = session
        .get(SESSION_MOBILE_PROVIDER_REDIRECT_URI)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;

    let Some(saved_csrf) = saved_csrf else {
        tracing::warn!("Mobile OIDC callback: no CSRF state in session");
        crate::metrics::record_auth_attempt("mobile_oidc", "failure", "missing_state");
        clear_mobile_oidc_session(&session).await?;
        return Ok(mobile_redirect_error(&app_redirect_uri, "missing_state"));
    };
    if state != saved_csrf {
        tracing::warn!("Mobile OIDC callback: CSRF state mismatch");
        crate::metrics::record_auth_attempt("mobile_oidc", "failure", "csrf");
        clear_mobile_oidc_session(&session).await?;
        return Ok(mobile_redirect_error(&app_redirect_uri, "csrf"));
    }

    let Some(nonce_str) = saved_nonce else {
        crate::metrics::record_auth_attempt("mobile_oidc", "failure", "missing_nonce");
        clear_mobile_oidc_session(&session).await?;
        return Ok(mobile_redirect_error(&app_redirect_uri, "missing_nonce"));
    };
    let Some(pkce_str) = saved_pkce else {
        crate::metrics::record_auth_attempt("mobile_oidc", "failure", "missing_pkce");
        clear_mobile_oidc_session(&session).await?;
        return Ok(mobile_redirect_error(&app_redirect_uri, "missing_pkce"));
    };
    let Some(provider_redirect_uri) = provider_redirect_uri else {
        crate::metrics::record_auth_attempt("mobile_oidc", "failure", "missing_redirect_uri");
        clear_mobile_oidc_session(&session).await?;
        return Ok(mobile_redirect_error(
            &app_redirect_uri,
            "missing_redirect_uri",
        ));
    };

    let (config, _) = AppConfig::load_with_db(&db).await;
    if !config.auth_sso_enabled
        || config.oidc_issuer.is_empty()
        || config.oidc_client_id.is_empty()
        || config.oidc_client_secret.is_empty()
    {
        crate::metrics::record_auth_attempt("mobile_oidc", "failure", "not_configured");
        clear_mobile_oidc_session(&session).await?;
        return Ok(mobile_redirect_error(
            &app_redirect_uri,
            "sso_not_configured",
        ));
    }

    let http = oidc_http_client();
    let client = match get_or_refresh_provider(&config, &http).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Mobile OIDC provider error during callback: {e}");
            crate::metrics::record_auth_attempt("mobile_oidc", "failure", "provider_error");
            clear_mobile_oidc_session(&session).await?;
            return Ok(mobile_redirect_error(&app_redirect_uri, "provider_error"));
        }
    };
    let redirect_url = RedirectUrl::new(provider_redirect_uri)
        .map_err(|e| cot::Error::internal(format!("bad mobile redirect URI from session: {e}")))?;
    let client = client.set_redirect_uri(redirect_url);

    let token_request = match client.exchange_code(AuthorizationCode::new(code)) {
        Ok(req) => req,
        Err(e) => {
            tracing::error!("Mobile OIDC token endpoint not configured: {e}");
            crate::metrics::record_auth_attempt("mobile_oidc", "failure", "token_config");
            clear_mobile_oidc_session(&session).await?;
            return Ok(mobile_redirect_error(&app_redirect_uri, "oidc_error"));
        }
    };
    let token_response = token_request
        .set_pkce_verifier(PkceCodeVerifier::new(pkce_str))
        .request_async(&http)
        .await;
    let token_response = match token_response {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("Mobile OIDC token exchange failed: {e}");
            crate::metrics::record_auth_attempt("mobile_oidc", "failure", "token_exchange");
            clear_mobile_oidc_session(&session).await?;
            return Ok(mobile_redirect_error(&app_redirect_uri, "oidc_error"));
        }
    };

    use openidconnect::TokenResponse;
    let id_token = match token_response.id_token() {
        Some(t) => t,
        None => {
            tracing::error!("Mobile OIDC response missing ID token");
            crate::metrics::record_auth_attempt("mobile_oidc", "failure", "missing_id_token");
            clear_mobile_oidc_session(&session).await?;
            return Ok(mobile_redirect_error(&app_redirect_uri, "oidc_error"));
        }
    };

    let nonce = Nonce::new(nonce_str);
    let claims = match id_token.claims(&client.id_token_verifier(), &nonce) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Mobile OIDC ID token verification failed: {e}");
            crate::metrics::record_auth_attempt("mobile_oidc", "failure", "id_token_verify");
            clear_mobile_oidc_session(&session).await?;
            return Ok(mobile_redirect_error(&app_redirect_uri, "oidc_error"));
        }
    };

    let sub = claims.subject().to_string();
    let issuer = claims.issuer().to_string();
    let email = claims.email().map(|e| e.to_string());
    let name = claims
        .name()
        .and_then(|n| n.get(None))
        .map(|n| n.to_string());
    let groups = extract_groups_from_jwt(&id_token.to_string());

    if !is_allowed_by_groups(&groups, &config.oidc_user_groups, &config.oidc_admin_groups) {
        tracing::warn!(
            "Mobile OIDC login denied by group allowlist: sub={sub}, groups={groups:?}, user_groups={:?}, admin_groups={:?}",
            config.oidc_user_groups,
            config.oidc_admin_groups,
        );
        crate::metrics::record_auth_attempt("mobile_oidc", "failure", "not_in_group");
        clear_mobile_oidc_session(&session).await?;
        return Ok(mobile_redirect_error(&app_redirect_uri, "access_denied"));
    }

    let user = match provision_user(
        &db,
        &issuer,
        &sub,
        email.as_deref(),
        name.as_deref(),
        &groups,
        &config.oidc_admin_groups,
    )
    .await
    {
        Ok(u) => u,
        Err(e) => {
            tracing::error!("Mobile OIDC user provisioning failed: {e}");
            crate::metrics::record_auth_attempt("mobile_oidc", "failure", "provisioning");
            clear_mobile_oidc_session(&session).await?;
            return Ok(mobile_redirect_error(&app_redirect_uri, "oidc_error"));
        }
    };

    let exchange_code = auth::create_mobile_exchange_code(&db, user.id_val())
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    clear_mobile_oidc_session(&session).await?;

    crate::metrics::record_auth_attempt("mobile_oidc", "success", "ok");
    crate::metrics::record_session_created("mobile_oidc");
    Ok(mobile_redirect_success(&app_redirect_uri, &exchange_code))
}

// ---------------------------------------------------------------------------
// User provisioning
// ---------------------------------------------------------------------------

/// Resolve the role based on OIDC group membership.
/// If `admin_groups` is non-empty and any user group matches, return "admin";
/// otherwise return "user".
fn resolve_role(groups: &[String], admin_groups: &str) -> &'static str {
    if admin_groups.is_empty() {
        return auth::Role::User.code();
    }
    let admin_set: std::collections::HashSet<&str> = admin_groups
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if admin_set.is_empty() {
        return auth::Role::User.code();
    }
    for g in groups {
        if admin_set.contains(g.as_str()) {
            return auth::Role::Admin.code();
        }
    }
    auth::Role::User.code()
}

fn parse_group_set(groups: &str) -> std::collections::HashSet<&str> {
    groups
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

fn has_any_group(groups: &[String], allowed: &std::collections::HashSet<&str>) -> bool {
    groups.iter().any(|g| allowed.contains(g.as_str()))
}

fn is_allowed_by_groups(groups: &[String], user_groups: &str, admin_groups: &str) -> bool {
    let user_set = parse_group_set(user_groups);
    if user_set.is_empty() {
        return true;
    }
    let admin_set = parse_group_set(admin_groups);
    has_any_group(groups, &user_set) || has_any_group(groups, &admin_set)
}

async fn provision_user(
    db: &Database,
    issuer: &str,
    sub: &str,
    email: Option<&str>,
    name: Option<&str>,
    groups: &[String],
    admin_groups: &str,
) -> Result<User, String> {
    let role = resolve_role(groups, admin_groups);

    // 1. Check for existing OIDC link.
    if let Some(mut link) = OidcLink::find_by_issuer_sub(db, issuer, sub)
        .await
        .map_err(|e| format!("DB error finding OIDC link: {e}"))?
    {
        // Fetch the linked user.
        match User::get_by_id(db, link.user_id()).await {
            Ok(Some(mut user)) => {
                // Update cached claims.
                link.update_claims(db, email, name)
                    .await
                    .map_err(|e| format!("DB error updating OIDC link: {e}"))?;

                // Always update role on login.
                user.update_role(db, role)
                    .await
                    .map_err(|e| format!("DB error updating user role: {e}"))?;

                return Ok(user);
            }
            Ok(None) => {
                // User was deleted but the OIDC link is stale — remove it
                // and fall through to re-create the user below.
                tracing::warn!(
                    "OIDC link points to deleted user {}; removing stale link",
                    link.user_id(),
                );
                link.delete(db)
                    .await
                    .map_err(|e| format!("DB error deleting stale OIDC link: {e}"))?;
            }
            Err(e) => return Err(format!("DB error fetching user: {e}")),
        }
    }

    // 2. No existing link — try to find a user by email.
    if let Some(email_str) = email {
        if let Some(mut user) = User::get_by_email(db, email_str)
            .await
            .map_err(|e| format!("DB error finding user by email: {e}"))?
        {
            // Create OIDC link for existing user.
            OidcLink::create_link(db, user.id_val(), issuer, sub, email, name)
                .await
                .map_err(|e| format!("DB error creating OIDC link: {e}"))?;

            user.update_role(db, role)
                .await
                .map_err(|e| format!("DB error updating user role: {e}"))?;

            return Ok(user);
        }
    }

    // 3. Create a brand-new user + OIDC link.
    // Generate a unique username from the sub or email.
    let username = if let Some(email_str) = email {
        email_str.split('@').next().unwrap_or(sub).to_owned()
    } else {
        sub.to_owned()
    };

    // Ensure username uniqueness by appending a suffix if needed.
    let mut candidate = username.clone();
    let mut suffix = 0u32;
    loop {
        match User::get_by_username(db, &candidate).await {
            Ok(None) => break,
            Ok(Some(_)) => {
                suffix += 1;
                candidate = format!("{username}_{suffix}");
            }
            Err(e) => return Err(format!("DB error checking username: {e}")),
        }
    }

    let user = User::create_oidc(db, &candidate, email, name, role)
        .await
        .map_err(|e| format!("DB error creating user: {e}"))?;

    OidcLink::create_link(db, user.id_val(), issuer, sub, email, name)
        .await
        .map_err(|e| format!("DB error creating OIDC link: {e}"))?;

    Ok(user)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn redirect_login_with_error(message: &str) -> cot::Result<cot::response::Response> {
    let encoded = urlencoded(message);
    Ok(auth::redirect(&format!("/login?error={encoded}")))
}

fn text_response(status: cot::http::StatusCode, message: &str) -> cot::response::Response {
    cot::http::Response::builder()
        .status(status)
        .body(cot::Body::fixed(message.to_owned()))
        .expect("valid response")
}

async fn mobile_app_redirect_uri_from_session(session: &Session) -> cot::Result<String> {
    let saved: Option<String> = session
        .get(SESSION_MOBILE_APP_REDIRECT_URI)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    Ok(safe_mobile_redirect_uri(saved.as_deref())
        .unwrap_or_else(|| DEFAULT_MOBILE_REDIRECT_URI.to_owned()))
}

async fn clear_mobile_oidc_session(session: &Session) -> cot::Result<()> {
    let _: Option<String> = session
        .remove(SESSION_MOBILE_CSRF_STATE)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    let _: Option<String> = session
        .remove(SESSION_MOBILE_NONCE)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    let _: Option<String> = session
        .remove(SESSION_MOBILE_PKCE_VERIFIER)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    let _: Option<String> = session
        .remove(SESSION_MOBILE_PROVIDER_REDIRECT_URI)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    let _: Option<String> = session
        .remove(SESSION_MOBILE_APP_REDIRECT_URI)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    Ok(())
}

fn safe_mobile_redirect_uri(raw: Option<&str>) -> Option<String> {
    let value = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_MOBILE_REDIRECT_URI);
    if value.len() > 2048 || value.bytes().any(|b| matches!(b, b'\r' | b'\n')) {
        return None;
    }

    let lower = value.to_ascii_lowercase();
    if lower.starts_with("furumi://") || lower.starts_with("furumusic://") {
        return Some(value.to_owned());
    }
    None
}

fn mobile_redirect_success(app_redirect_uri: &str, code: &str) -> cot::response::Response {
    auth::redirect(&append_query_param(app_redirect_uri, "code", code))
}

fn mobile_redirect_error(app_redirect_uri: &str, error: &str) -> cot::response::Response {
    auth::redirect(&append_query_param(app_redirect_uri, "error", error))
}

fn append_query_param(uri: &str, key: &str, value: &str) -> String {
    let (base, fragment) = uri.split_once('#').unwrap_or((uri, ""));
    let separator = if base.contains('?') { '&' } else { '?' };
    let mut out = format!("{base}{separator}{key}={}", urlencoded(value));
    if !fragment.is_empty() {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

fn extract_groups_from_jwt(token: &str) -> Vec<String> {
    use base64::Engine;

    let Some(payload_b64) = token.split('.').nth(1) else {
        return Vec::new();
    };
    let Ok(payload_bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload_b64))
    else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&payload_bytes) else {
        return Vec::new();
    };
    let Some(arr) = value.get("groups").and_then(|value| value.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|value| value.as_str().map(String::from))
        .collect()
}

/// Minimal percent-encoding for query parameter values.
fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}
