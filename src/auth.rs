use cot::Body;
use cot::db::Database;
use cot::response::IntoResponse;
use cot::session::Session;

use crate::user::User;

// ---------------------------------------------------------------------------
// Role enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Admin,
    User,
}

impl Role {
    pub fn code(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::User => "user",
        }
    }

    pub fn from_code(s: &str) -> Option<Self> {
        match s {
            "admin" => Some(Role::Admin),
            "user" => Some(Role::User),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Session-based auth
// ---------------------------------------------------------------------------

const SESSION_USER_ID: &str = "user_id";

#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    pub id: i64,
    pub name: String,
    pub role: Role,
}

/// Read `user_id` from the session, fetch the `User` from DB, return
/// `AuthenticatedUser` if the user exists and is active.
pub async fn get_session_user(session: &Session, db: &Database) -> Option<AuthenticatedUser> {
    let user_id: i64 = session.get(SESSION_USER_ID).await.ok()??;
    let user = User::get_by_id(db, user_id).await.ok()??;
    if !user.is_active() {
        return None;
    }
    let name = {
        let display = user.display_name_str();
        if display.is_empty() {
            user.username_str().to_owned()
        } else {
            display
        }
    };
    crate::metrics::record_active_user(user.id_val());
    Some(AuthenticatedUser {
        id: user.id_val(),
        name,
        role: user.role(),
    })
}

/// Return `Ok(user)` if the session belongs to an active admin, otherwise
/// `Err(response)` — a redirect to `/login` or a 403.
pub async fn require_admin_or_redirect(
    session: &Session,
    db: &Database,
) -> Result<AuthenticatedUser, cot::response::Response> {
    let Some(user) = get_session_user(session, db).await else {
        crate::metrics::record_authorization_denied("unauthenticated");
        return Err(redirect("/login"));
    };
    if user.role != Role::Admin {
        crate::metrics::record_authorization_denied("forbidden");
        return Err("Forbidden"
            .with_status(cot::http::StatusCode::FORBIDDEN)
            .into_response()
            .expect("valid response"));
    }
    Ok(user)
}

/// Insert user_id into the session and cycle the session ID.
pub async fn login(session: &Session, user_id: i64) -> cot::Result<()> {
    session
        .cycle_id()
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    session
        .insert(SESSION_USER_ID, user_id)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    crate::metrics::record_active_user(user_id);
    Ok(())
}

/// Flush (destroy) the session.
pub async fn logout(session: &Session) -> cot::Result<()> {
    session
        .flush()
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    Ok(())
}

/// Build a 303 See Other redirect response.
pub fn redirect(location: &str) -> cot::response::Response {
    cot::http::Response::builder()
        .status(cot::http::StatusCode::SEE_OTHER)
        .header(cot::http::header::LOCATION, location)
        .body(Body::fixed(""))
        .expect("valid response")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_roundtrip() {
        assert_eq!(Role::from_code("admin"), Some(Role::Admin));
        assert_eq!(Role::from_code("user"), Some(Role::User));
        assert_eq!(Role::from_code("other"), None);
        assert_eq!(Role::Admin.code(), "admin");
        assert_eq!(Role::User.code(), "user");
    }
}
