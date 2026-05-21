pub mod views;

use std::sync::Arc;

use cot::db::Database;
use cot::db::migrations::SyncDynMigration;
use cot::request::extractors::{Path, RequestForm, UrlQuery};
use cot::response::IntoResponse;
use cot::router::method::get;
use cot::router::{Route, Router};
use cot::session::Session;
use cot::App;
use serde::Deserialize;

use crate::auth;
use crate::config::AppConfig;
use crate::i18n::I18n;
use crate::user::User;
use views::{OidcSettingsForm, SetupForm, UserForm};

/// Build-time metadata baked in by `build.rs` and Cargo env vars.
#[derive(Debug)]
pub struct BuildInfo {
    pub pkg_name: &'static str,
    pub pkg_version: &'static str,
    pub profile: &'static str,
    pub target: &'static str,
    pub rustc_version: &'static str,
}

pub static BUILD_INFO: BuildInfo = BuildInfo {
    pkg_name: env!("CARGO_PKG_NAME"),
    pkg_version: env!("CARGO_PKG_VERSION"),
    profile: if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    },
    target: env!("FURU_TARGET"),
    rustc_version: env!("FURU_RUSTC_VERSION"),
};

pub struct AdminApp {
    config: Arc<AppConfig>,
}

impl AdminApp {
    pub fn new(config: Arc<AppConfig>) -> Self {
        Self { config }
    }
}

#[derive(Debug, Deserialize)]
struct SettingsQuery {
    saved: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PathId {
    id: i64,
}

impl App for AdminApp {
    fn name(&self) -> &'static str {
        "admin"
    }

    fn router(&self) -> Router {
        Router::with_urls([
            // -- Setup (first-run, no auth required) --------------------------
            Route::with_handler_and_name(
                "/setup",
                get(|i18n: I18n, db: Database| async move {
                    let count = User::count_all(&db).await.unwrap_or(1);
                    if count > 0 {
                        return Ok(auth::redirect("/admin/"));
                    }
                    views::setup_page(i18n, String::new())
                        .await?
                        .into_response()
                })
                .post(
                    |i18n: I18n, db: Database, session: Session,
                     form: RequestForm<SetupForm>| async move {
                        let count = User::count_all(&db).await.unwrap_or(1);
                        if count > 0 {
                            return Ok(auth::redirect("/admin/"));
                        }
                        views::setup_submit(i18n, &db, &session, form).await
                    },
                ),
                "admin_setup",
            ),
            // -- Dashboard ----------------------------------------------------
            Route::with_handler_and_name(
                "/",
                |session: Session, db: Database, i18n: I18n| async move {
                    // First-run redirect
                    let count = User::count_all(&db).await.unwrap_or(0);
                    if count == 0 {
                        return Ok(auth::redirect("/admin/setup"));
                    }
                    let admin = match auth::require_admin_or_redirect(&session, &db).await {
                        Ok(u) => u,
                        Err(resp) => return Ok(resp),
                    };
                    views::admin_index(admin, i18n).await?.into_response()
                },
                "admin_index",
            ),
            // -- Debug --------------------------------------------------------
            Route::with_handler_and_name(
                "/debug",
                {
                    let config = Arc::clone(&self.config);
                    move |session: Session, db: Database, i18n: I18n| {
                        let config = Arc::clone(&config);
                        async move {
                            let admin =
                                match auth::require_admin_or_redirect(&session, &db).await {
                                    Ok(u) => u,
                                    Err(resp) => return Ok(resp),
                                };
                            views::debug_handler(admin, i18n, &config, &db)
                                .await?
                                .into_response()
                        }
                    }
                },
                "admin_debug",
            ),
            // -- Settings -----------------------------------------------------
            Route::with_handler_and_name(
                "/settings",
                get({
                    let config = Arc::clone(&self.config);
                    move |session: Session, db: Database, i18n: I18n,
                          query: UrlQuery<SettingsQuery>| {
                        let config = Arc::clone(&config);
                        async move {
                            let admin =
                                match auth::require_admin_or_redirect(&session, &db).await {
                                    Ok(u) => u,
                                    Err(resp) => return Ok(resp),
                                };
                            let saved = query.0.saved.as_deref() == Some("1");
                            views::settings_handler(admin, i18n, &config, &db, saved)
                                .await?
                                .into_response()
                        }
                    }
                })
                .post({
                    let config = Arc::clone(&self.config);
                    move |session: Session, db: Database, i18n: I18n,
                          form: RequestForm<OidcSettingsForm>| {
                        let config = Arc::clone(&config);
                        async move {
                            let admin =
                                match auth::require_admin_or_redirect(&session, &db).await {
                                    Ok(u) => u,
                                    Err(resp) => return Ok(resp),
                                };
                            views::settings_submit(admin, i18n, &config, &db, form).await
                        }
                    }
                }),
                "admin_settings",
            ),
            // -- Users --------------------------------------------------------
            Route::with_handler_and_name(
                "/users",
                |session: Session, db: Database, i18n: I18n| async move {
                    let admin = match auth::require_admin_or_redirect(&session, &db).await {
                        Ok(u) => u,
                        Err(resp) => return Ok(resp),
                    };
                    views::users_list(admin, i18n, &db).await?.into_response()
                },
                "admin_users",
            ),
            Route::with_handler_and_name(
                "/users/new",
                get(|session: Session, db: Database, i18n: I18n| async move {
                    let admin = match auth::require_admin_or_redirect(&session, &db).await {
                        Ok(u) => u,
                        Err(resp) => return Ok(resp),
                    };
                    views::users_new(admin, i18n).await?.into_response()
                })
                .post(
                    |session: Session, db: Database, form: RequestForm<UserForm>| async move {
                        let admin = match auth::require_admin_or_redirect(&session, &db).await {
                            Ok(u) => u,
                            Err(resp) => return Ok(resp),
                        };
                        views::users_create(admin, &db, form).await
                    },
                ),
                "admin_users_new",
            ),
            Route::with_handler_and_name(
                "/users/{id}/edit",
                get(
                    |session: Session, db: Database, i18n: I18n,
                     path: Path<PathId>| async move {
                        let admin = match auth::require_admin_or_redirect(&session, &db).await {
                            Ok(u) => u,
                            Err(resp) => return Ok(resp),
                        };
                        views::users_edit(admin, i18n, &db, path.0.id)
                            .await?
                            .into_response()
                    },
                )
                .post(
                    |session: Session, db: Database, path: Path<PathId>,
                     form: RequestForm<UserForm>| async move {
                        let admin = match auth::require_admin_or_redirect(&session, &db).await {
                            Ok(u) => u,
                            Err(resp) => return Ok(resp),
                        };
                        views::users_update(admin, &db, path.0.id, form).await
                    },
                ),
                "admin_users_edit",
            ),
            Route::with_handler_and_name(
                "/users/{id}/delete",
                cot::router::method::post(
                    |session: Session, db: Database, path: Path<PathId>| async move {
                        let admin = match auth::require_admin_or_redirect(&session, &db).await {
                            Ok(u) => u,
                            Err(resp) => return Ok(resp),
                        };
                        views::users_delete(admin, &db, path.0.id).await
                    },
                ),
                "admin_users_delete",
            ),
        ])
    }

    fn migrations(&self) -> Vec<Box<SyncDynMigration>> {
        let mut all =
            cot::db::migrations::wrap_migrations(crate::config::db_migrations::MIGRATIONS);
        all.extend(cot::db::migrations::wrap_migrations(
            crate::user::db_migrations::MIGRATIONS,
        ));
        all
    }
}
