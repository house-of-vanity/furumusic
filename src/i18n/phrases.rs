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

    // Artist management
    nav_artists:              "Artists"                       , "Артисты";
    artists_heading:          "Artists"                       , "Артисты";
    artists_add:              "Add artist"                    , "Добавить артиста";
    artists_name:             "Name"                          , "Имя";
    artists_hidden:           "Hidden"                        , "Скрыт";
    artists_actions:          "Actions"                       , "Действия";
    artists_edit:             "Edit"                          , "Редактировать";
    artists_delete:           "Delete"                        , "Удалить";
    artists_delete_confirm:   "Are you sure?"                 , "Вы уверены?";
    artists_new_heading:      "New artist"                    , "Новый артист";
    artists_edit_heading:     "Edit artist"                   , "Редактирование артиста";
    artists_empty:            "No artists yet."               , "Артистов пока нет.";
    artists_releases:         "Releases"                      , "Релизы";
    artists_tracks:           "Tracks"                        , "Треки";
    artists_view_releases:    "View releases"                 , "Показать релизы";
    artists_image:            "Artist Image"                  , "Изображение артиста";
    artists_no_image:         "No image set."                 , "Изображение не задано.";
    artists_upload_image:     "Upload custom image"           , "Загрузить изображение";
    artists_upload:           "Upload"                        , "Загрузить";
    artists_pick_cover:       "Or pick from album covers"     , "Или выберите обложку альбома";
    artists_no_covers:        "No album covers available."    , "Обложки альбомов недоступны.";
    artists_remove_image:     "Remove image"                  , "Удалить изображение";

    // Release management
    nav_releases:             "Releases"                       , "Релизы";
    releases_heading:         "Releases"                       , "Релизы";
    releases_add:             "Add release"                    , "Добавить релиз";
    releases_title:           "Title"                          , "Название";
    releases_type:            "Type"                           , "Тип";
    releases_year:            "Year"                           , "Год";
    releases_artist:          "Artist"                         , "Артист";
    releases_artists:         "Artists"                        , "Артисты";
    releases_hidden:          "Hidden"                         , "Скрыт";
    releases_actions:         "Actions"                        , "Действия";
    releases_edit:            "Edit"                           , "Редактировать";
    releases_delete:          "Delete"                         , "Удалить";
    releases_delete_confirm:  "Are you sure?"                  , "Вы уверены?";
    releases_new_heading:     "New release"                    , "Новый релиз";
    releases_edit_heading:    "Edit release"                   , "Редактирование релиза";
    releases_empty:           "No releases yet."               , "Релизов пока нет.";
    releases_no_artist:       "— no artist —"                  , "— без артиста —";
    releases_select_artist:   "Select artist..."               , "Выберите артиста...";
    releases_filter_all:      "All artists"                    , "Все артисты";
    releases_filter_label:    "Filter by artist"               , "Фильтр по артисту";

    // Media files
    nav_media_files:          "Media Files"                    , "Медиафайлы";
    media_files_heading:      "Media Files"                    , "Медиафайлы";
    media_files_empty:        "No media files found."          , "Медиафайлы не найдены.";
    media_files_filename:     "Filename"                       , "Файл";
    media_files_type:         "Type"                           , "Тип";
    media_files_format:       "Format"                         , "Формат";
    media_files_size:         "Size"                           , "Размер";
    media_files_path:         "Path"                           , "Путь";
    media_files_hash:         "SHA-256"                        , "SHA-256";
    media_files_created:      "Created"                        , "Создан";
    media_files_track:        "Track"                          , "Трек";
    media_files_orphan:       "Orphan"                         , "Без трека";
    media_files_actions:      "Actions"                        , "Действия";
    media_files_delete:       "Delete"                         , "Удалить";
    media_files_delete_confirm: "Delete this media file?"      , "Удалить этот медиафайл?";

    // Job management
    nav_jobs:                 "Jobs"                             , "Задания";
    nav_reviews:              "Reviews"                          , "Проверки";
    jobs_heading:             "Scheduled Jobs"                   , "Запланированные задания";
    jobs_name:                "Name"                             , "Имя";
    jobs_description:         "Description"                      , "Описание";
    jobs_cron:                "Cron"                             , "Cron";
    jobs_enabled:             "Enabled"                          , "Включено";
    jobs_last_run:            "Last run"                         , "Последний запуск";
    jobs_next_run:            "Next run"                         , "Следующий запуск";
    jobs_actions:             "Actions"                          , "Действия";
    jobs_run_now:             "Run now"                          , "Запустить";
    jobs_enable:              "Enable"                           , "Включить";
    jobs_disable:             "Disable"                          , "Выключить";
    jobs_run_history:         "Run history"                      , "История запусков";
    jobs_run_status:          "Status"                           , "Статус";
    jobs_run_started:         "Started"                          , "Начало";
    jobs_run_duration:        "Duration"                         , "Длительность";
    jobs_run_trigger:         "Trigger"                          , "Триггер";
    jobs_run_log:             "Log"                              , "Лог";
    jobs_run_error:           "Error"                            , "Ошибка";
    jobs_cron_help:           "7-field cron: sec min hour day month weekday year" , "7-полевой cron: сек мин час день месяц день_недели год";
    jobs_cron_update:         "Update cron"                      , "Обновить cron";
    jobs_back_to_list:        "Back to jobs"                     , "Назад к заданиям";
    jobs_run_detail:          "Run detail"                       , "Детали запуска";
    jobs_back_to_job:         "Back to job"                      , "Назад к заданию";
    jobs_metadata_backfill_options: "Metadata backfill options"   , "Параметры обновления метадаты";
    jobs_metadata_backfill_fields: "Fields to update"             , "Поля для обновления";
    jobs_metadata_backfill_fill_missing: "Fill missing only"      , "Заполнить только пустые";
    jobs_metadata_backfill_overwrite: "Overwrite existing values" , "Перезаписать существующие";
    jobs_metadata_backfill_run: "Run metadata backfill"           , "Запустить обновление метадаты";

    // Review management
    reviews_heading:          "Pending Reviews"                  , "Ожидающие проверки";
    reviews_empty:            "No reviews."                      , "Проверок нет.";
    reviews_status:           "Status"                           , "Статус";
    reviews_type:             "Type"                             , "Тип";
    reviews_input_path:       "Input"                            , "Файл";
    reviews_tags:             "Tags"                             , "Теги";
    reviews_confidence:       "Confidence"                       , "Уверенность";
    reviews_approve:          "Approve"                          , "Подтвердить";
    reviews_reject:           "Reject"                           , "Отклонить";
    reviews_context:          "Context"                          , "Контекст";
    reviews_result:           "Result"                           , "Результат";
    reviews_created:          "Created"                          , "Создано";
    reviews_view:             "View"                             , "Открыть";
    reviews_clear_all:        "Clear all"                        , "Очистить все";
    reviews_clear_filtered:   "Clear shown"                      , "Очистить показанные";
    reviews_clear_confirm:    "Are you sure? This will delete the selected reviews." , "Вы уверены? Выбранные проверки будут удалены.";
    reviews_select_all:       "Select shown"                     , "Выбрать показанные";
    reviews_clear_selection:  "Clear selection"                  , "Снять выбор";
    reviews_delete_selected:  "Delete selected"                  , "Удалить выбранные";
    reviews_requeue_selected: "Re-queue selected"                , "В очередь выбранные";
    reviews_selected_none:    "Selected: 0"                      , "Выбрано: 0";
    reviews_selected_prefix:  "Selected"                         , "Выбрано";
    reviews_none_selected_confirm: "Select at least one review."  , "Выберите хотя бы одну проверку.";
    reviews_delete_selected_confirm: "Delete selected reviews?"   , "Удалить выбранные проверки?";
    reviews_requeue_selected_confirm: "Re-queue selected reviews?" , "Поставить выбранные проверки в очередь?";
    reviews_back_to_list:     "Back to reviews"                  , "Назад к проверкам";
    reviews_filter_all:       "All"                              , "Все";
    reviews_filter_pending:   "Pending"                          , "Ожидают";
    reviews_filter_approved:  "Approved"                         , "Подтверждённые";
    reviews_filter_rejected:  "Rejected"                         , "Отклонённые";
    reviews_filter_queued:    "Queued"                            , "В очереди";
    reviews_filter_processing: "Processing"                      , "В обработке";
    reviews_filter_auto_approved: "Auto-approved"                , "Авто-подтверждённые";
    reviews_filter_failed:    "Failed"                            , "Ошибочные";
    reviews_error:            "Error"                             , "Ошибка";
    reviews_requeue:          "Re-queue"                          , "В очередь";
    reviews_requeue_confirm:  "Re-queue this item for processing?" , "Поставить в очередь на повторную обработку?";

    // Processing stats
    settings_agent_concurrency: "Concurrency"                     , "Параллелизм";

    reviews_model:            "Model"                             , "Модель";
    reviews_llm_duration:     "LLM time"                          , "Время LLM";
    reviews_tokens:           "Tokens (in/out)"                   , "Токены (вх/вых)";

    // Agent settings
    settings_agent:           "Agent"                           , "Агент";
    settings_agent_help:      "AI music processing agent configuration. Enable and configure the background agent that automatically processes audio files." , "Настройки AI-агента обработки музыки. Включите и настройте фоновый агент, который автоматически обрабатывает аудиофайлы.";
    settings_agent_enabled:   "Agent enabled"                   , "Агент включён";
    settings_agent_inbox:     "Inbox directory"                 , "Папка входящих";
    settings_agent_storage:   "Storage directory"               , "Папка хранилища";
    settings_agent_llm_url:   "LLM API URL"                    , "URL API LLM";
    settings_agent_llm_model: "LLM model"                      , "Модель LLM";
    settings_agent_threshold: "Confidence threshold"            , "Порог уверенности";
    settings_agent_context:   "Context limit (tokens)"          , "Лимит контекста (токены)";
    settings_agent_llm_auth:  "LLM auth header"                 , "Заголовок авторизации LLM";
    settings_agent_status:    "Agent Status"                    , "Статус агента";
    settings_agent_status_disabled: "Agent is disabled."        , "Агент отключён.";
    settings_agent_status_no_url: "LLM URL is not configured."  , "URL LLM не настроен.";
    settings_agent_status_ok:     "LLM connection OK"           , "Подключение к LLM OK";
    settings_agent_status_error:  "LLM connection error"        , "Ошибка подключения к LLM";
    settings_agent_model_name:    "Model"                       , "Модель";
    settings_agent_latency:       "Latency"                     , "Задержка";
    settings_agent_prompt_tokens: "Prompt tokens"               , "Токенов на промпт";
    settings_agent_completion_tokens: "Completion tokens"       , "Токенов на ответ";
    settings_agent_tokens_per_sec: "Tokens/sec"                 , "Токенов/сек";
    settings_agent_status_loading: "Checking connection"         , "Проверка подключения";
}
