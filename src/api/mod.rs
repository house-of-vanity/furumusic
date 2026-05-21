use cot::db::Database;
use cot::router::method::get;
use cot::router::{Route, Router};
use cot::session::Session;
use cot::{App, Body};

use crate::auth;

// ---------------------------------------------------------------------------
// JSON response helpers
// ---------------------------------------------------------------------------

fn json_ok(value: &serde_json::Value) -> cot::response::Response {
    cot::http::Response::builder()
        .status(cot::http::StatusCode::OK)
        .header(cot::http::header::CONTENT_TYPE, "application/json")
        .body(Body::fixed(value.to_string()))
        .expect("valid response")
}

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

async fn me_handler(
    session: Session,
    db: Database,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(
            cot::http::StatusCode::UNAUTHORIZED,
            "not authenticated",
        ));
    };

    Ok(json_ok(&serde_json::json!({
        "id": user.id,
        "name": user.name,
        "role": user.role.code(),
    })))
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
            Route::with_handler_and_name("/me", get(me_handler), "api_me"),
        ])
    }
}
