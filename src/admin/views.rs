use cot::db::{Database, Model};
use cot::form::{Form, FormResult};
use cot::html::Html;
use cot::request::extractors::RequestForm;
use cot::response::IntoResponse;
use cot::session::Session;
use cot::{Body, Template};

use crate::auth::{self, AuthenticatedUser};
use crate::config::{AppConfig, ConfigEntry, ConfigSources};
use crate::i18n::{I18n, Translations};
use crate::user::User;
use super::BUILD_INFO;

/// A config entry for display in the unified debug table.
#[derive(Debug)]
pub struct ConfigDisplayEntry {
    pub key: String,
    pub env_var: String,
    pub value: String,
    pub default_value: String,
    pub source: &'static str,
}

/// Secret field names that should be redacted in the debug view.
const SECRET_FIELDS: &[&str] = &[
    "database_url",
    "oidc_client_secret",
];

fn is_secret(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SECRET_FIELDS.iter().any(|s| lower.contains(s))
        || lower.contains("secret")
        || lower.contains("token")
}

fn redact(value: &str) -> String {
    if value.is_empty() {
        String::new()
    } else {
        "********".into()
    }
}

#[derive(Debug, Template)]
#[template(path = "admin/debug.html")]
struct DebugTemplate<'a> {
    t: &'static Translations,
    user_name: String,
    user_role: String,
    build: &'a super::BuildInfo,
    config_entries: Vec<ConfigDisplayEntry>,
    db_status: String,
}

fn config_display_entries(config: &AppConfig, sources: &ConfigSources) -> Vec<ConfigDisplayEntry> {
    let defaults = AppConfig::default();

    macro_rules! entry {
        ($field:ident, $value:expr, $default:expr) => {
            {
                let raw = $value;
                let default_raw = $default;
                let secret = is_secret(stringify!($field));
                let display = if secret { redact(&raw) } else { raw };
                let default_display = if secret { redact(&default_raw) } else { default_raw };
                ConfigDisplayEntry {
                    key: stringify!($field).into(),
                    env_var: format!("FURU_{}", stringify!($field).to_ascii_uppercase()),
                    value: display,
                    default_value: default_display,
                    source: sources.$field.code(),
                }
            }
        };
    }

    vec![
        entry!(database_url, config.database_url.clone(), defaults.database_url.clone()),
        entry!(oidc_issuer, config.oidc_issuer.clone(), defaults.oidc_issuer.clone()),
        entry!(oidc_client_id, config.oidc_client_id.clone(), defaults.oidc_client_id.clone()),
        entry!(oidc_client_secret, config.oidc_client_secret.clone(), defaults.oidc_client_secret.clone()),
        entry!(log_level, config.log_level.clone(), defaults.log_level.clone()),
        entry!(auth_password_enabled, config.auth_password_enabled.to_string(), defaults.auth_password_enabled.to_string()),
        entry!(auth_sso_enabled, config.auth_sso_enabled.to_string(), defaults.auth_sso_enabled.to_string()),
        entry!(oidc_button_text, config.oidc_button_text.clone(), defaults.oidc_button_text.clone()),
        entry!(oidc_admin_groups, config.oidc_admin_groups.clone(), defaults.oidc_admin_groups.clone()),
    ]
}

pub async fn debug_handler(
    admin: AuthenticatedUser,
    i18n: I18n,
    _startup_config: &AppConfig,
    db: &Database,
) -> cot::Result<Html> {
    let (config, sources) = AppConfig::load_with_db(db).await;

    let db_status = match db.raw("SELECT 1").await {
        Ok(_) => i18n.t.debug_db_connected.to_owned(),
        Err(e) => format!("{}: {e}", i18n.t.debug_db_error),
    };

    let template = DebugTemplate {
        t: i18n.t,
        user_name: admin.name,
        user_role: admin.role.code().to_owned(),
        build: &BUILD_INFO,
        config_entries: config_display_entries(&config, &sources),
        db_status,
    };
    Ok(Html::new(template.render()?))
}

#[derive(Debug, Template)]
#[template(path = "admin/index.html")]
struct AdminIndexTemplate {
    t: &'static Translations,
    user_name: String,
    user_role: String,
}

pub async fn admin_index(admin: AuthenticatedUser, i18n: I18n) -> cot::Result<Html> {
    let template = AdminIndexTemplate {
        t: i18n.t,
        user_name: admin.name,
        user_role: admin.role.code().to_owned(),
    };
    Ok(Html::new(template.render()?))
}

// ---------------------------------------------------------------------------
// Settings page
// ---------------------------------------------------------------------------

#[derive(Debug, Template)]
#[template(path = "admin/settings.html")]
struct SettingsTemplate {
    t: &'static Translations,
    user_name: String,
    user_role: String,
    saved: bool,
    auth_password_enabled: bool,
    auth_password_enabled_source: &'static str,
    auth_sso_enabled: bool,
    auth_sso_enabled_source: &'static str,
    oidc_button_text: String,
    oidc_button_text_source: &'static str,
    oidc_issuer: String,
    oidc_issuer_source: &'static str,
    oidc_client_id: String,
    oidc_client_id_source: &'static str,
    oidc_client_secret: String,
    oidc_client_secret_source: &'static str,
    oidc_admin_groups: String,
    oidc_admin_groups_source: &'static str,
}

pub async fn settings_handler(
    admin: AuthenticatedUser,
    i18n: I18n,
    _startup_config: &AppConfig,
    db: &Database,
    saved: bool,
) -> cot::Result<Html> {
    let (config, sources) = AppConfig::load_with_db(db).await;

    let template = SettingsTemplate {
        t: i18n.t,
        user_name: admin.name,
        user_role: admin.role.code().to_owned(),
        saved,
        auth_password_enabled: config.auth_password_enabled,
        auth_password_enabled_source: sources.auth_password_enabled.code(),
        auth_sso_enabled: config.auth_sso_enabled,
        auth_sso_enabled_source: sources.auth_sso_enabled.code(),
        oidc_button_text: config.oidc_button_text,
        oidc_button_text_source: sources.oidc_button_text.code(),
        oidc_issuer: config.oidc_issuer,
        oidc_issuer_source: sources.oidc_issuer.code(),
        oidc_client_id: config.oidc_client_id,
        oidc_client_id_source: sources.oidc_client_id.code(),
        oidc_client_secret: config.oidc_client_secret,
        oidc_client_secret_source: sources.oidc_client_secret.code(),
        oidc_admin_groups: config.oidc_admin_groups,
        oidc_admin_groups_source: sources.oidc_admin_groups.code(),
    };
    Ok(Html::new(template.render()?))
}

#[derive(Debug, Form)]
pub struct OidcSettingsForm {
    auth_password_enabled: Option<String>,
    auth_sso_enabled: Option<String>,
    oidc_button_text: String,
    oidc_issuer: String,
    oidc_client_id: String,
    oidc_client_secret: String,
    oidc_admin_groups: String,
}

pub async fn settings_submit(
    _admin: AuthenticatedUser,
    _i18n: I18n,
    _startup_config: &AppConfig,
    db: &Database,
    form: RequestForm<OidcSettingsForm>,
) -> cot::Result<cot::http::Response<Body>> {
    let RequestForm(result) = form;
    match result {
        FormResult::Ok(data) => {
            let pw_enabled = if data.auth_password_enabled.is_some() { "true" } else { "false" };
            let sso_enabled = if data.auth_sso_enabled.is_some() { "true" } else { "false" };
            let fields: [(&str, &str); 7] = [
                ("auth_password_enabled", pw_enabled),
                ("auth_sso_enabled", sso_enabled),
                ("oidc_button_text", &data.oidc_button_text),
                ("oidc_issuer", &data.oidc_issuer),
                ("oidc_client_id", &data.oidc_client_id),
                ("oidc_client_secret", &data.oidc_client_secret),
                ("oidc_admin_groups", &data.oidc_admin_groups),
            ];
            for (key, value) in fields {
                let mut entry = ConfigEntry::new(key.to_owned(), value.to_owned());
                if let Err(e) = entry.save(db).await {
                    tracing::error!(key, error = %e, "failed to save config entry");
                    return Err(e.into());
                }
            }

            Ok(auth::redirect("/admin/settings?saved=1"))
        }
        FormResult::ValidationError(_ctx) => {
            Ok(auth::redirect("/admin/settings"))
        }
    }
}

// ---------------------------------------------------------------------------
// User management
// ---------------------------------------------------------------------------

#[derive(Debug, Template)]
#[template(path = "admin/users.html")]
struct UsersTemplate {
    t: &'static Translations,
    user_name: String,
    user_role: String,
    users: Vec<User>,
}

pub async fn users_list(admin: AuthenticatedUser, i18n: I18n, db: &Database) -> cot::Result<Html> {
    let users = User::list_all(db).await.unwrap_or_default();
    let template = UsersTemplate {
        t: i18n.t,
        user_name: admin.name,
        user_role: admin.role.code().to_owned(),
        users,
    };
    Ok(Html::new(template.render()?))
}

#[derive(Debug, Template)]
#[template(path = "admin/user_form.html")]
struct UserFormTemplate {
    t: &'static Translations,
    user_name: String,
    user_role: String,
    is_edit: bool,
    form_user_id: i64,
    form_username: String,
    form_email: String,
    form_display_name: String,
    form_role: String,
}

pub async fn users_new(admin: AuthenticatedUser, i18n: I18n) -> cot::Result<Html> {
    let template = UserFormTemplate {
        t: i18n.t,
        user_name: admin.name,
        user_role: admin.role.code().to_owned(),
        is_edit: false,
        form_user_id: 0,
        form_username: String::new(),
        form_email: String::new(),
        form_display_name: String::new(),
        form_role: "user".into(),
    };
    Ok(Html::new(template.render()?))
}

#[derive(Debug, Form)]
pub struct UserForm {
    username: String,
    email: String,
    display_name: String,
    password: String,
    role: String,
}

pub async fn users_create(
    _admin: AuthenticatedUser,
    db: &Database,
    form: RequestForm<UserForm>,
) -> cot::Result<cot::http::Response<Body>> {
    let RequestForm(result) = form;
    match result {
        FormResult::Ok(data) => {
            let email = if data.email.is_empty() { None } else { Some(data.email.as_str()) };
            let display_name = if data.display_name.is_empty() { None } else { Some(data.display_name.as_str()) };
            User::create(db, &data.username, email, display_name, &data.password, &data.role).await
                .map_err(|e| cot::Error::internal(format!("failed to create user: {e}")))?;
            Ok(auth::redirect("/admin/users"))
        }
        FormResult::ValidationError(_) => {
            Ok(auth::redirect("/admin/users/new"))
        }
    }
}

pub async fn users_edit(
    admin: AuthenticatedUser,
    i18n: I18n,
    db: &Database,
    user_id: i64,
) -> cot::Result<Html> {
    let target = User::get_by_id(db, user_id).await
        .map_err(|e| cot::Error::internal(format!("db error: {e}")))?
        .ok_or_else(|| cot::Error::internal("user not found"))?;
    let template = UserFormTemplate {
        t: i18n.t,
        user_name: admin.name,
        user_role: admin.role.code().to_owned(),
        is_edit: true,
        form_user_id: target.id_val(),
        form_username: target.username_str().to_owned(),
        form_email: target.email_str(),
        form_display_name: target.display_name_str(),
        form_role: target.role_str().to_owned(),
    };
    Ok(Html::new(template.render()?))
}

pub async fn users_update(
    _admin: AuthenticatedUser,
    db: &Database,
    user_id: i64,
    form: RequestForm<UserForm>,
) -> cot::Result<cot::http::Response<Body>> {
    let RequestForm(result) = form;
    match result {
        FormResult::Ok(data) => {
            let mut target = User::get_by_id(db, user_id).await
                .map_err(|e| cot::Error::internal(format!("db error: {e}")))?
                .ok_or_else(|| cot::Error::internal("user not found"))?;
            let email = if data.email.is_empty() { None } else { Some(data.email.as_str()) };
            let display_name = if data.display_name.is_empty() { None } else { Some(data.display_name.as_str()) };
            let new_password = if data.password.is_empty() { None } else { Some(data.password.as_str()) };
            target.update_fields(db, &data.username, email, display_name, new_password, &data.role).await
                .map_err(|e| cot::Error::internal(format!("failed to update user: {e}")))?;
            Ok(auth::redirect("/admin/users"))
        }
        FormResult::ValidationError(_) => {
            Ok(auth::redirect(&format!("/admin/users/{user_id}/edit")))
        }
    }
}

pub async fn users_delete(
    _admin: AuthenticatedUser,
    db: &Database,
    user_id: i64,
) -> cot::Result<cot::http::Response<Body>> {
    User::delete_by_id(db, user_id).await
        .map_err(|e| cot::Error::internal(format!("failed to delete user: {e}")))?;
    Ok(auth::redirect("/admin/users"))
}

// ---------------------------------------------------------------------------
// First-run setup page
// ---------------------------------------------------------------------------

#[derive(Debug, Template)]
#[template(path = "admin/setup.html")]
struct SetupTemplate {
    t: &'static Translations,
    message: String,
}

pub async fn setup_page(i18n: I18n, message: String) -> cot::Result<Html> {
    let template = SetupTemplate {
        t: i18n.t,
        message,
    };
    Ok(Html::new(template.render()?))
}

#[derive(Debug, Form)]
pub struct SetupForm {
    username: String,
    password: String,
    confirm_password: String,
}

pub async fn setup_submit(
    i18n: I18n,
    db: &Database,
    session: &Session,
    form: RequestForm<SetupForm>,
) -> cot::Result<cot::response::Response> {
    let RequestForm(result) = form;
    let data = match result {
        FormResult::Ok(data) => data,
        FormResult::ValidationError(_) => {
            return setup_page(i18n, String::new()).await?.into_response();
        }
    };

    if data.password != data.confirm_password {
        let msg = i18n.t.setup_mismatch.to_owned();
        return setup_page(i18n, msg).await?.into_response();
    }

    let user = User::create(db, &data.username, None, None, &data.password, "admin")
        .await
        .map_err(|e| cot::Error::internal(format!("failed to create admin: {e}")))?;

    auth::login(session, user.id_val()).await?;
    Ok(auth::redirect("/admin/"))
}
