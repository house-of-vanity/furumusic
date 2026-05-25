use cot::db::Database;
use cot::json::Json;
use cot::response::IntoResponse;
use cot::router::method::openapi::api_get;
use cot::router::{Route, Router};
use cot::session::Session;
use cot::{App, Body};
use schemars::JsonSchema;
use serde::Serialize;

use crate::auth;

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

// ---------------------------------------------------------------------------
// GET /api/me
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, JsonSchema)]
struct MeResponse {
    id: i64,
    name: String,
    role: String,
}

async fn me_handler(session: Session, db: Database) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
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

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

pub struct ApiApp;

impl App for ApiApp {
    fn name(&self) -> &'static str {
        "api"
    }

    fn router(&self) -> Router {
        Router::with_urls([Route::with_api_handler_and_name(
            "/me",
            api_get(me_handler),
            "api_me",
        )])
    }
}
