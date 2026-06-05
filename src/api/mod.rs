use std::marker::PhantomData;

use cot::aide::openapi::{
    MediaType, Operation, ReferenceOr, RequestBody, Response as OpenApiResponse, SchemaObject,
    StatusCode as OpenApiStatusCode,
};
use cot::auth::PasswordVerificationResult;
use cot::common_types::Password;
use cot::db::Database;
use cot::http::StatusCode;
use cot::http::header::CONTENT_TYPE;
use cot::json::Json;
use cot::openapi::{AsApiOperation, RouteContext};
use cot::response::IntoResponse;
use cot::router::method::openapi::{api_get, api_post};
use cot::router::{Route, Router};
use cot::session::Session;
use cot::{App, Body, RequestHandler};
use schemars::{JsonSchema, SchemaGenerator};
use serde::{Deserialize, Serialize};

use crate::auth;
use crate::config::AppConfig;
use crate::user::User;

// ---------------------------------------------------------------------------
// JSON error helper
// ---------------------------------------------------------------------------

fn json_error(status: cot::http::StatusCode, message: &str) -> cot::response::Response {
    let body = serde_json::json!({ "error": message });
    cot::http::Response::builder()
        .status(status)
        .header(cot::http::header::CONTENT_TYPE, "application/json")
        .body(Body::fixed(body.to_string()))
        .expect("valid response")
}

#[derive(Clone, Copy)]
struct DocumentedJsonHandler<H, Req, Res> {
    handler: H,
    summary: &'static str,
    _marker: PhantomData<fn(Req) -> Res>,
}

#[derive(Clone, Copy)]
struct DocumentedResponseHandler<H, Res> {
    handler: H,
    summary: &'static str,
    _marker: PhantomData<fn() -> Res>,
}

fn documented_json_handler<Req, Res, H>(
    handler: H,
    summary: &'static str,
) -> DocumentedJsonHandler<H, Req, Res> {
    DocumentedJsonHandler {
        handler,
        summary,
        _marker: PhantomData,
    }
}

fn documented_response_handler<Res, H>(
    handler: H,
    summary: &'static str,
) -> DocumentedResponseHandler<H, Res> {
    DocumentedResponseHandler {
        handler,
        summary,
        _marker: PhantomData,
    }
}

impl<HandlerParams, H, Req, Res> RequestHandler<HandlerParams>
    for DocumentedJsonHandler<H, Req, Res>
where
    H: RequestHandler<HandlerParams> + Clone + Send + Sync + 'static,
{
    async fn handle(&self, request: cot::request::Request) -> cot::Result<cot::response::Response> {
        self.handler.handle(request).await
    }
}

impl<HandlerParams, H, Res> RequestHandler<HandlerParams> for DocumentedResponseHandler<H, Res>
where
    H: RequestHandler<HandlerParams> + Clone + Send + Sync + 'static,
{
    async fn handle(&self, request: cot::request::Request) -> cot::Result<cot::response::Response> {
        self.handler.handle(request).await
    }
}

impl<H, Req, Res> AsApiOperation for DocumentedJsonHandler<H, Req, Res>
where
    Req: JsonSchema,
    Res: JsonSchema,
{
    fn as_api_operation(
        &self,
        _route_context: &RouteContext<'_>,
        schema_generator: &mut SchemaGenerator,
    ) -> Option<Operation> {
        let mut operation = Operation {
            summary: Some(self.summary.to_owned()),
            ..Default::default()
        };

        let mut request_body = RequestBody {
            required: true,
            ..Default::default()
        };
        request_body.content.insert(
            "application/json".to_owned(),
            MediaType {
                schema: Some(SchemaObject {
                    json_schema: Req::json_schema(schema_generator),
                    external_docs: None,
                    example: None,
                }),
                ..Default::default()
            },
        );
        operation.request_body = Some(ReferenceOr::Item(request_body));

        let responses = operation.responses.get_or_insert_default();
        let mut ok = OpenApiResponse {
            description: "OK".to_owned(),
            ..Default::default()
        };
        ok.content.insert(
            "application/json".to_owned(),
            MediaType {
                schema: Some(SchemaObject {
                    json_schema: Res::json_schema(schema_generator),
                    external_docs: None,
                    example: None,
                }),
                ..Default::default()
            },
        );
        responses
            .responses
            .insert(OpenApiStatusCode::Code(200), ReferenceOr::Item(ok));

        Some(operation)
    }
}

impl<H, Res> AsApiOperation for DocumentedResponseHandler<H, Res>
where
    Res: JsonSchema,
{
    fn as_api_operation(
        &self,
        _route_context: &RouteContext<'_>,
        schema_generator: &mut SchemaGenerator,
    ) -> Option<Operation> {
        let mut operation = Operation {
            summary: Some(self.summary.to_owned()),
            ..Default::default()
        };
        add_json_response::<Res>(&mut operation, schema_generator);
        Some(operation)
    }
}

fn add_json_response<Res: JsonSchema>(
    operation: &mut Operation,
    schema_generator: &mut SchemaGenerator,
) {
    let responses = operation.responses.get_or_insert_default();
    let mut ok = OpenApiResponse {
        description: "OK".to_owned(),
        ..Default::default()
    };
    ok.content.insert(
        "application/json".to_owned(),
        MediaType {
            schema: Some(SchemaObject {
                json_schema: Res::json_schema(schema_generator),
                external_docs: None,
                example: None,
            }),
            ..Default::default()
        },
    );
    responses
        .responses
        .insert(OpenApiStatusCode::Code(200), ReferenceOr::Item(ok));
}

fn is_json_content_type(value: &str) -> bool {
    value
        .split(';')
        .next()
        .map(str::trim)
        .is_some_and(|media_type| media_type.eq_ignore_ascii_case("application/json"))
}

async fn parse_json_request<T>(
    request: cot::request::Request,
) -> cot::Result<Result<T, cot::response::Response>>
where
    T: for<'de> Deserialize<'de>,
{
    let content_type = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !is_json_content_type(content_type) {
        return Ok(Err(json_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "expected application/json",
        )));
    }

    let bytes = request.into_body().into_bytes().await?;
    let body = match serde_json::from_slice::<T>(&bytes) {
        Ok(body) => body,
        Err(_) => {
            return Ok(Err(json_error(
                StatusCode::BAD_REQUEST,
                "invalid JSON body",
            )));
        }
    };
    Ok(Ok(body))
}

// ---------------------------------------------------------------------------
// GET /api/me
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, JsonSchema)]
struct MeResponse {
    id: i64,
    name: String,
    role: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct AuthUserResponse {
    id: i64,
    name: String,
    role: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct AuthTokenResponse {
    access_token: String,
    refresh_token: String,
    token_type: String,
    expires_in_seconds: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
struct AuthLoginResponse {
    user: AuthUserResponse,
    tokens: AuthTokenResponse,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PasswordLoginRequest {
    username: String,
    password: String,
    device_name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RefreshRequest {
    refresh_token: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SsoExchangeRequest {
    code: String,
    device_name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LogoutRequest {
    refresh_token: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct LogoutResponse {
    revoked: bool,
}

fn user_response(user: auth::AuthenticatedUser) -> AuthUserResponse {
    AuthUserResponse {
        id: user.id,
        name: user.name,
        role: user.role.code().to_owned(),
    }
}

fn token_response(tokens: auth::ApiTokenPair) -> AuthTokenResponse {
    AuthTokenResponse {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        token_type: tokens.token_type.to_owned(),
        expires_in_seconds: tokens.expires_in_seconds,
    }
}

fn login_response(user: auth::AuthenticatedUser, tokens: auth::ApiTokenPair) -> AuthLoginResponse {
    AuthLoginResponse {
        user: user_response(user),
        tokens: token_response(tokens),
    }
}

async fn me_handler(
    auth_ctx: auth::AuthContext,
    session: Session,
    db: Database,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_request_user(&auth_ctx, &session, &db).await else {
        return Ok(json_error(
            cot::http::StatusCode::UNAUTHORIZED,
            "not authenticated",
        ));
    };

    Json(MeResponse {
        id: user.id,
        name: user.name,
        role: user.role.code().to_owned(),
    })
    .into_response()
}

async fn password_login_handler(
    db: Database,
    raw_request: cot::request::Request,
) -> cot::Result<cot::response::Response> {
    let request = match parse_json_request::<PasswordLoginRequest>(raw_request).await? {
        Ok(request) => request,
        Err(response) => return Ok(response),
    };

    let (config, _) = AppConfig::load_with_db(&db).await;
    if !config.auth_password_enabled {
        crate::metrics::record_auth_attempt("api_password", "failure", "disabled");
        return Ok(json_error(
            StatusCode::FORBIDDEN,
            "password login is disabled",
        ));
    }

    let user = match User::get_by_username(&db, request.username.trim()).await {
        Ok(Some(user)) if user.is_active() => user,
        _ => {
            crate::metrics::record_auth_attempt("api_password", "failure", "bad_credentials");
            return Ok(json_error(
                StatusCode::UNAUTHORIZED,
                "invalid username or password",
            ));
        }
    };

    let Some(hash) = user.password_ref() else {
        crate::metrics::record_auth_attempt("api_password", "failure", "bad_credentials");
        return Ok(json_error(
            StatusCode::UNAUTHORIZED,
            "invalid username or password",
        ));
    };

    match hash.verify(&Password::new(&request.password)) {
        PasswordVerificationResult::Ok | PasswordVerificationResult::OkObsolete(_) => {
            let auth_user = auth::AuthenticatedUser {
                id: user.id_val(),
                name: {
                    let display = user.display_name_str();
                    if display.is_empty() {
                        user.username_str().to_owned()
                    } else {
                        display
                    }
                },
                role: user.role(),
            };
            let tokens =
                auth::create_api_session(&db, user.id_val(), request.device_name.as_deref())
                    .await
                    .map_err(|e| cot::Error::internal(e.to_string()))?;
            crate::metrics::record_auth_attempt("api_password", "success", "ok");
            crate::metrics::record_session_created("api_password");
            Json(login_response(auth_user, tokens)).into_response()
        }
        PasswordVerificationResult::Invalid => {
            crate::metrics::record_auth_attempt("api_password", "failure", "bad_credentials");
            Ok(json_error(
                StatusCode::UNAUTHORIZED,
                "invalid username or password",
            ))
        }
    }
}

async fn refresh_handler(
    db: Database,
    raw_request: cot::request::Request,
) -> cot::Result<cot::response::Response> {
    let request = match parse_json_request::<RefreshRequest>(raw_request).await? {
        Ok(request) => request,
        Err(response) => return Ok(response),
    };

    match auth::refresh_api_session(&db, request.refresh_token.trim()).await {
        Ok(Some(tokens)) => Json(token_response(tokens)).into_response(),
        Ok(None) => Ok(json_error(
            StatusCode::UNAUTHORIZED,
            "invalid refresh token",
        )),
        Err(err) => Err(cot::Error::internal(err.to_string())),
    }
}

async fn sso_exchange_handler(
    db: Database,
    raw_request: cot::request::Request,
) -> cot::Result<cot::response::Response> {
    let request = match parse_json_request::<SsoExchangeRequest>(raw_request).await? {
        Ok(request) => request,
        Err(response) => return Ok(response),
    };

    match auth::exchange_mobile_code_for_api_session(
        &db,
        request.code.trim(),
        request.device_name.as_deref(),
    )
    .await
    {
        Ok(Some((user, tokens))) => {
            crate::metrics::record_auth_attempt("api_sso_exchange", "success", "ok");
            crate::metrics::record_session_created("api_sso_exchange");
            Json(login_response(user, tokens)).into_response()
        }
        Ok(None) => {
            crate::metrics::record_auth_attempt("api_sso_exchange", "failure", "bad_code");
            Ok(json_error(
                StatusCode::UNAUTHORIZED,
                "invalid SSO exchange code",
            ))
        }
        Err(err) => Err(cot::Error::internal(err.to_string())),
    }
}

async fn logout_handler(
    auth_ctx: auth::AuthContext,
    db: Database,
    raw_request: cot::request::Request,
) -> cot::Result<cot::response::Response> {
    let request = match parse_json_request::<LogoutRequest>(raw_request).await? {
        Ok(request) => request,
        Err(response) => return Ok(response),
    };

    let revoked = auth::revoke_api_session(
        &db,
        auth_ctx.bearer_token(),
        request.refresh_token.as_deref().map(str::trim),
    )
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;
    Json(LogoutResponse { revoked }).into_response()
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

pub struct ApiApp;

impl App for ApiApp {
    fn name(&self) -> &'static str {
        "api"
    }

    fn router(&self) -> Router {
        Router::with_urls([
            Route::with_api_handler_and_name(
                "/me",
                api_get(documented_response_handler::<MeResponse, _>(
                    me_handler,
                    "Get the current authenticated user",
                )),
                "api_me",
            ),
            Route::with_api_handler_and_name(
                "/auth/password",
                api_post(documented_json_handler::<
                    PasswordLoginRequest,
                    AuthLoginResponse,
                    _,
                >(
                    password_login_handler,
                    "Log in with username and password",
                )),
                "api_auth_password",
            ),
            Route::with_api_handler_and_name(
                "/auth/refresh",
                api_post(documented_json_handler::<
                    RefreshRequest,
                    AuthTokenResponse,
                    _,
                >(
                    refresh_handler, "Refresh an API token pair"
                )),
                "api_auth_refresh",
            ),
            Route::with_api_handler_and_name(
                "/auth/sso/exchange",
                api_post(documented_json_handler::<
                    SsoExchangeRequest,
                    AuthLoginResponse,
                    _,
                >(
                    sso_exchange_handler,
                    "Exchange a mobile SSO code for API tokens",
                )),
                "api_auth_sso_exchange",
            ),
            Route::with_api_handler_and_name(
                "/auth/logout",
                api_post(documented_json_handler::<LogoutRequest, LogoutResponse, _>(
                    logout_handler,
                    "Revoke an API session",
                )),
                "api_auth_logout",
            ),
        ])
    }
}

#[cfg(test)]
mod tests {
    use cot::aide::openapi::{PathItem, ReferenceOr};

    use super::*;

    fn assert_get_path(paths: &cot::aide::openapi::Paths, path: &str) {
        assert!(matches!(
            paths.paths.get(path),
            Some(ReferenceOr::Item(PathItem { get: Some(_), .. }))
        ));
    }

    fn assert_post_path(paths: &cot::aide::openapi::Paths, path: &str) {
        assert!(matches!(
            paths.paths.get(path),
            Some(ReferenceOr::Item(PathItem { post: Some(_), .. }))
        ));
    }

    #[test]
    fn openapi_includes_auth_routes() {
        let openapi = ApiApp.router().as_api();
        let paths = openapi.paths.expect("OpenAPI paths");

        assert_get_path(&paths, "/me");
        assert_post_path(&paths, "/auth/password");
        assert_post_path(&paths, "/auth/refresh");
        assert_post_path(&paths, "/auth/sso/exchange");
        assert_post_path(&paths, "/auth/logout");

        let Some(ReferenceOr::Item(PathItem {
            post: Some(operation),
            ..
        })) = paths.paths.get("/auth/password")
        else {
            panic!("password auth path should be documented as POST");
        };
        assert!(operation.request_body.is_some());
        assert!(operation.responses.is_some());
    }
}
