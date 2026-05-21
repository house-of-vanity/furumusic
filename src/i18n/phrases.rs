use super::translations;

translations! {
    // Global
    site_name:            "furumusic"                    , "furumusic";

    // Navigation / sidebar
    nav_admin:            "admin"                        , "админка";
    nav_dashboard:        "Dashboard"                    , "Панель управления";
    nav_debug:            "Debug"                        , "Отладка";

    // Index page
    index_heading:        "furumusic"                    , "furumusic";
    index_status:         "server is running"            , "сервер запущен";

    // Admin index
    admin_heading:        "Admin"                        , "Админка";
    admin_debug_link:     "Debug info"                   , "Отладочная информация";

    // Debug page
    debug_heading:        "Debug Information"             , "Отладочная информация";
    debug_build_info:     "Build Info"                    , "Информация о сборке";
    debug_app_config:     "App Config"                    , "Конфигурация";
    debug_field:          "Field"                         , "Поле";
    debug_value:          "Value"                         , "Значение";
    debug_source:         "Source"                        , "Источник";

    // Navigation (settings)
    nav_settings:         "Settings"                      , "Настройки";

    // Debug page — DB status
    debug_db_status:      "Database"                      , "База данных";
    debug_db_connected:   "connected"                     , "подключена";
    debug_db_error:       "error"                         , "ошибка";

    // Settings page
    settings_heading:     "Settings"                      , "Настройки";
    settings_oidc:        "OIDC Configuration"            , "Настройки OIDC";
    settings_save:        "Save"                          , "Сохранить";
    settings_saved:       "Settings saved."               , "Настройки сохранены.";

    // Auth settings
    settings_auth:            "Authentication"              , "Аутентификация";
    settings_password_login:  "Password login"              , "Вход по паролю";
    settings_sso_login:       "SSO login"                   , "Вход через SSO";
    settings_oidc_button:     "SSO button text"             , "Текст кнопки SSO";

    // Login page
    login_heading:            "Sign in"                     , "Вход";
    login_username:           "Username"                    , "Имя пользователя";
    login_password:           "Password"                    , "Пароль";
    login_submit:             "Sign in"                     , "Войти";
    login_disabled:           "Login is currently disabled." , "Вход сейчас отключён.";
    login_invalid:            "Invalid username or password." , "Неверное имя пользователя или пароль.";

    // Logout
    nav_logout:               "Logout"                      , "Выход";

    // Setup page
    setup_heading:            "Create Admin Account"        , "Создание аккаунта администратора";
    setup_username:           "Username"                    , "Имя пользователя";
    setup_password:           "Password"                    , "Пароль";
    setup_confirm:            "Confirm password"            , "Подтверждение пароля";
    setup_submit:             "Create"                      , "Создать";
    setup_mismatch:           "Passwords do not match."     , "Пароли не совпадают.";

    // OIDC help
    settings_oidc_help:       "Register this application with your identity provider. Use the callback URL shown below as the Redirect URI." , "Зарегистрируйте это приложение у вашего провайдера идентификации. Используйте указанный ниже callback URL в качестве Redirect URI.";
    settings_oidc_callback:   "Callback URL"                  , "Callback URL";
    settings_oidc_issuer_help: "Base URL of the OIDC provider (e.g. https://accounts.google.com)" , "Базовый URL провайдера OIDC (напр. https://accounts.google.com)";
    settings_oidc_admin_groups: "Admin groups"                   , "Группы администраторов";
    settings_oidc_admin_groups_help: "Comma-separated OIDC group names that grant admin role (e.g. /admin,/furumusic-admins)" , "OIDC группы через запятую, дающие роль администратора (напр. /admin,/furumusic-admins)";

    // User management
    nav_users:                "Users"                       , "Пользователи";
    users_heading:            "Users"                       , "Пользователи";
    users_add:                "Add user"                    , "Добавить пользователя";
    users_username:           "Username"                    , "Имя пользователя";
    users_email:              "Email"                       , "Email";
    users_display_name:       "Display name"                , "Отображаемое имя";
    users_role:               "Role"                        , "Роль";
    users_active:             "Active"                      , "Активен";
    users_actions:            "Actions"                     , "Действия";
    users_edit:               "Edit"                        , "Редактировать";
    users_delete:             "Delete"                      , "Удалить";
    users_delete_confirm:     "Are you sure?"               , "Вы уверены?";
    users_new_heading:        "New user"                    , "Новый пользователь";
    users_edit_heading:       "Edit user"                   , "Редактирование пользователя";
    users_password_hint:      "Leave blank to keep current" , "Оставьте пустым, чтобы не менять";
    users_saved:              "User saved."                 , "Пользователь сохранён.";

    // API settings
    settings_api:             "API"                           , "API";
    settings_swagger:         "Swagger UI"                    , "Swagger UI";
    settings_swagger_help:    "Serves interactive API docs at /swagger/ (requires restart)" , "Интерактивная документация API на /swagger/ (требуется перезапуск)";

    // OIDC login errors
    login_oidc_error:         "SSO login failed. Please try again." , "Ошибка входа через SSO. Попробуйте ещё раз.";
    login_sso_disabled:       "SSO login is not configured." , "Вход через SSO не настроен.";
}
