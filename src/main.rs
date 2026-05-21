mod admin;
mod api;
mod auth;
mod config;
mod i18n;
mod oidc;
mod user;

use std::sync::Arc;

use cot::auth::PasswordVerificationResult;
use cot::cli::CliMetadata;
use cot::common_types::Password;
use cot::config::{
    DatabaseConfig, MiddlewareConfig, ProjectConfig, SessionMiddlewareConfig, SessionStoreConfig,
    SessionStoreTypeConfig,
};
use cot::db::Database;
use cot::form::{Form, FormResult};
use cot::html::Html;
use cot::middleware::SessionMiddleware;
use cot::project::RegisterAppsContext;
use cot::request::extractors::{RequestForm, UrlQuery};
use cot::response::IntoResponse;
use cot::router::method::get;
use cot::router::{Route, Router};
use cot::session::Session;
use cot::{App, AppBuilder, Body, Project, Template};
use serde::Deserialize;

use crate::config::AppConfig;
use crate::i18n::{I18n, Translations};
use crate::user::User;

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn index(
    session: Session,
    db: Database,
    i18n: I18n,
) -> cot::Result<cot::response::Response> {
    let user = match auth::get_session_user(&session, &db).await {
        Some(u) => u,
        None => return Ok(auth::redirect("/login")),
    };
    let role_label = match user.role {
        auth::Role::Admin => format!(
            r#"{} | <a href="/admin/">{}</a>"#,
            user.role.code(),
            i18n.t.nav_admin
        ),
        _ => user.role.code().to_owned(),
    };
    Html::new(format!(
        "<h1>{}</h1><p>{}</p><p>{}: {}</p>",
        i18n.t.index_heading, i18n.t.index_status, user.name, role_label
    ))
    .into_response()
}

#[derive(Deserialize)]
struct SetLangQuery {
    lang: String,
    next: Option<String>,
}

async fn set_lang(
    UrlQuery(query): UrlQuery<SetLangQuery>,
) -> cot::Result<cot::http::Response<Body>> {
    let lang = i18n::Lang::from_code(&query.lang).unwrap_or(i18n::Lang::En);
    let next = query.next.as_deref().unwrap_or("/");

    let response = cot::http::Response::builder()
        .status(cot::http::StatusCode::SEE_OTHER)
        .header(cot::http::header::LOCATION, next)
        .header(cot::http::header::SET_COOKIE, i18n::lang_cookie(lang))
        .body(Body::fixed(""))
        .expect("valid response");

    Ok(response)
}

// ---------------------------------------------------------------------------
// Login page
// ---------------------------------------------------------------------------

#[derive(Debug, Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    t: &'static Translations,
    auth_password_enabled: bool,
    auth_sso_enabled: bool,
    oidc_button_text: String,
    message: String,
}

async fn login_page_handler(
    i18n: I18n,
    _startup_config: &AppConfig,
    db: Database,
    message: String,
) -> cot::Result<Html> {
    let (config, _) = AppConfig::load_with_db(&db).await;
    let template = LoginTemplate {
        t: i18n.t,
        auth_password_enabled: config.auth_password_enabled,
        auth_sso_enabled: config.auth_sso_enabled,
        oidc_button_text: config.oidc_button_text,
        message,
    };
    Ok(Html::new(template.render()?))
}

#[derive(Debug, Form)]
struct LoginForm {
    username: String,
    password: String,
}

// ---------------------------------------------------------------------------
// Logout
// ---------------------------------------------------------------------------

async fn logout_handler(session: Session) -> cot::Result<cot::response::Response> {
    auth::logout(&session).await?;
    Ok(auth::redirect("/login"))
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

struct FuruApp {
    config: Arc<AppConfig>,
}

impl App for FuruApp {
    fn name(&self) -> &'static str {
        env!("CARGO_PKG_NAME")
    }

    fn router(&self) -> Router {
        Router::with_urls([
            Route::with_handler_and_name(
                "/admin",
                get(|| async { Ok::<_, cot::Error>(auth::redirect("/admin/")) }),
                "admin_redirect",
            ),
            Route::with_handler_and_name("/", index, "index"),
            Route::with_handler_and_name(
                "/login",
                get({
                    let config = Arc::clone(&self.config);
                    move |i18n: I18n, db: Database| {
                        let config = Arc::clone(&config);
                        async move {
                            // No users at all → redirect to first-run setup
                            if User::count_all(&db).await.unwrap_or(0) == 0 {
                                return Ok(auth::redirect("/admin/setup"));
                            }
                            login_page_handler(i18n, &config, db, String::new())
                                .await?
                                .into_response()
                        }
                    }
                }).post({
                    let config = Arc::clone(&self.config);
                    move |i18n: I18n, db: Database, session: Session,
                          form: RequestForm<LoginForm>| {
                        let config = Arc::clone(&config);
                        async move {
                            let RequestForm(result) = form;
                            let data = match result {
                                FormResult::Ok(data) => data,
                                FormResult::ValidationError(_) => {
                                    let msg = i18n.t.login_invalid.to_owned();
                                    return login_page_handler(i18n, &config, db, msg)
                                        .await?
                                        .into_response();
                                }
                            };

                            // Try to authenticate
                            if let Ok(Some(user)) =
                                User::get_by_username(&db, &data.username).await
                            {
                                if let Some(hash) = user.password_ref() {
                                    let password = Password::new(&data.password);
                                    match hash.verify(&password) {
                                        PasswordVerificationResult::Ok
                                        | PasswordVerificationResult::OkObsolete(_) => {
                                            auth::login(&session, user.id_val()).await?;
                                            return Ok(auth::redirect("/"));
                                        }
                                        PasswordVerificationResult::Invalid => {}
                                    }
                                }
                            }

                            let msg = i18n.t.login_invalid.to_owned();
                            login_page_handler(i18n, &config, db, msg)
                                .await?
                                .into_response()
                        }
                    }
                }),
                "login",
            ),
            Route::with_handler_and_name("/logout", get(logout_handler), "logout"),
            Route::with_handler_and_name("/set-lang", set_lang, "set_lang"),
            Route::with_handler_and_name(
                "/auth/oidc/start",
                get(oidc::oidc_start_handler),
                "oidc_start",
            ),
            Route::with_handler_and_name(
                "/auth/oidc/callback",
                get(oidc::oidc_callback_handler),
                "oidc_callback",
            ),
        ])
    }
}

// ---------------------------------------------------------------------------
// Project
// ---------------------------------------------------------------------------

struct FuruProject {
    app_config: Arc<AppConfig>,
}

impl Project for FuruProject {
    fn cli_metadata(&self) -> CliMetadata {
        CliMetadata {
            description: concat!(
                env!("CARGO_PKG_DESCRIPTION"),
                "\n\n",
                "CONFIGURATION\n",
                "  All settings are available as FURU_-prefixed environment variables.\n",
                "  Priority: env var > DB override > compiled default.\n",
                "\n",
                "  Database (required for most features):\n",
                "    FURU_DATABASE_URL    PostgreSQL connection URL\n",
                "                         Example: postgres://user:pass@localhost/furumusic\n",
                "\n",
                "  Server:\n",
                "    FURU_LOG_LEVEL       Tracing filter (default: info)\n",
                "\n",
                "  Authentication:\n",
                "    FURU_AUTH_PASSWORD_ENABLED  Enable password login (default: true)\n",
                "    FURU_AUTH_SSO_ENABLED       Enable SSO/OIDC login (default: false)\n",
                "    FURU_OIDC_ISSUER            OIDC issuer URL\n",
                "    FURU_OIDC_CLIENT_ID         OIDC client ID\n",
                "    FURU_OIDC_CLIENT_SECRET      OIDC client secret\n",
                "    FURU_OIDC_BUTTON_TEXT        SSO button label (default: Sign in with SSO)\n",
                "    FURU_OIDC_ADMIN_GROUPS       OIDC groups that grant admin role\n",
                "\n",
                "QUICK START\n",
                "  export FURU_DATABASE_URL=postgres://user:pass@localhost/furumusic\n",
                "  furumusic run",
            ),
            ..cot::cli::metadata!()
        }
    }

    fn config(&self, _config_name: &str) -> cot::Result<ProjectConfig> {
        let mut builder = ProjectConfig::builder();
        builder.debug(cfg!(debug_assertions));

        if !self.app_config.database_url.is_empty() {
            builder.database(
                DatabaseConfig::builder()
                    .url(self.app_config.database_url.as_str())
                    .build(),
            );
            builder.middlewares(
                MiddlewareConfig::builder()
                    .session(
                        SessionMiddlewareConfig::builder()
                            .store(
                                SessionStoreConfig::builder()
                                    .store_type(SessionStoreTypeConfig::Database)
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            );
        }

        Ok(builder.build())
    }

    fn middlewares(
        &self,
        handler: cot::project::RootHandlerBuilder,
        context: &cot::project::MiddlewareContext,
    ) -> cot::project::RootHandler {
        handler
            .middleware(
                SessionMiddleware::from_context(context)
                    .same_site(cot::config::SameSite::Lax),
            )
            .build()
    }

    fn register_apps(&self, apps: &mut AppBuilder, _context: &RegisterAppsContext) {
        apps.register(cot::session::db::SessionApp::new());
        apps.register_with_views(
            FuruApp {
                config: Arc::clone(&self.app_config),
            },
            "",
        );
        apps.register_with_views(
            admin::AdminApp::new(Arc::clone(&self.app_config)),
            "/admin",
        );
        apps.register_with_views(api::ApiApp, "/api");
    }
}

// ---------------------------------------------------------------------------
// Entrypoint
// ---------------------------------------------------------------------------

#[cot::main]
fn main() -> impl Project {
    let app_config = Arc::new(AppConfig::load());

    // Initialise tracing subscriber with the configured log level.
    // FURU_LOG_LEVEL (or the default "info") is parsed as an EnvFilter
    // directive, so values like "debug", "warn,furumusic=trace" all work.
    let filter = tracing_subscriber::EnvFilter::try_new(&app_config.log_level)
        .unwrap_or_else(|e| {
            eprintln!(
                "WARNING: invalid FURU_LOG_LEVEL {:?}: {e}; falling back to \"info\"",
                app_config.log_level,
            );
            tracing_subscriber::EnvFilter::new("info")
        });
    tracing_subscriber::fmt().with_env_filter(filter).init();

    tracing::info!("loaded config: {:?}", app_config);

    FuruProject { app_config }
}
