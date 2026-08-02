pub mod browser_metadata;
mod clipboard;
pub mod diagnostics;
mod domain;
mod jobs;
mod migration;
pub mod native_host;
mod persistence;
mod settings;

use browser_metadata::MetadataBuffer;
use chrono::Utc;
use clipboard::ClipboardAccess;
use domain::{Category, ClipQuery, ClipSummary, OwnCopyGuard};
use parking_lot::Mutex;
use persistence::Repository;
use serde::Serialize;
use settings::{Settings, SettingsStore};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

struct AppState {
    repo: Arc<Repository>,
    settings: Arc<SettingsStore>,
    guard: Arc<OwnCopyGuard>,
    clipboard: Arc<ClipboardAccess>,
    paused: Arc<AtomicBool>,
    quitting: AtomicBool,
    pause_item: Mutex<Option<MenuItem<tauri::Wry>>>,
    /// The shortcut currently registered at runtime, tracked for rollback.
    registered_shortcut: Mutex<Option<String>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Bootstrap {
    clips: Vec<ClipSummary>,
    categories: Vec<Category>,
    settings: Settings,
    invalid_settings_warning: bool,
}

type CommandResult<T> = Result<T, String>;
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
fn bootstrap(state: tauri::State<AppState>, popup: bool) -> CommandResult<Bootstrap> {
    let limit = if popup { 16 } else { 60 };
    Ok(Bootstrap {
        clips: state
            .repo
            .list_clips(&ClipQuery {
                limit: Some(limit),
                ..Default::default()
            })
            .map_err(err)?,
        categories: state.repo.list_categories().map_err(err)?,
        settings: state.settings.get(),
        invalid_settings_warning: state.settings.has_invalid_warning(),
    })
}

/// Returns true if the invalid-settings warning should be shown, and clears the flag.
/// Call once per window; subsequent calls return false.
#[tauri::command]
fn consume_invalid_settings_warning(state: tauri::State<AppState>) -> bool {
    state.settings.consume_invalid_warning()
}

#[tauri::command]
fn list_clips(state: tauri::State<AppState>, query: ClipQuery) -> CommandResult<Vec<ClipSummary>> {
    state.repo.list_clips(&query).map_err(err)
}

#[tauri::command]
fn get_clip_content(state: tauri::State<AppState>, id: String) -> CommandResult<String> {
    state.repo.get_clip_content(&id).map_err(err)
}

/// Copy a clip to the system clipboard.
///
/// Operation order (safe):
/// 1. Read clip content from DB.
/// 2. Mark expected own-copy in guard.
/// 3. Write to system clipboard.
/// 4. Only on success: update copy stats in DB.
/// 5. Emit clips-changed.
/// 6. Hide popup (only if all steps succeeded).
///
/// If clipboard write fails → DB is NOT updated, popup stays open, error returned.
/// If DB update fails after clipboard write → error logged, returned; popup stays open.
#[tauri::command]
fn copy_clip(
    app: AppHandle,
    state: tauri::State<AppState>,
    id: String,
    popup: Option<bool>,
) -> CommandResult<()> {
    // Step 1: read content (does not mutate anything).
    let content = state.repo.get_clip_content(&id).map_err(err)?;

    // Steps 2+3: set clipboard (marks pending guard, commits on success, cancels on fail).
    clipboard::set_clipboard(&content, &state.guard, &state.clipboard).map_err(err)?;

    // Step 4: update DB stats only after clipboard write succeeded.
    let now = Utc::now().timestamp_millis();
    if let Err(e) = state.repo.mark_clip_copied(&id, now) {
        log::error!("copy_clip: clipboard set but DB update failed: {e}");
        return Err(err(e));
    }

    // Step 5: notify UI.
    let _ = app.emit("clips-changed", ());

    // Step 6: hide popup only on full success.
    if popup.unwrap_or(false) {
        if let Some(w) = app.get_webview_window("popup") {
            let _ = w.hide();
        }
    }
    Ok(())
}

#[tauri::command]
fn delete_clip(app: AppHandle, state: tauri::State<AppState>, id: String) -> CommandResult<()> {
    let res = state.repo.delete_clip(&id).map_err(err);
    if res.is_ok() {
        let _ = app.emit("clips-changed", ());
    }
    res
}

#[tauri::command]
fn set_pinned(
    app: AppHandle,
    state: tauri::State<AppState>,
    id: String,
    pinned: bool,
) -> CommandResult<()> {
    let res = state.repo.set_pinned(&id, pinned).map_err(err);
    if res.is_ok() {
        let _ = app.emit("clips-changed", ());
    }
    res
}

#[tauri::command]
fn clear_unpinned(app: AppHandle, state: tauri::State<AppState>) -> CommandResult<usize> {
    let res = state.repo.clear_unpinned().map_err(err);
    if res.is_ok() {
        let _ = app.emit("clips-changed", ());
    }
    res
}

#[tauri::command]
fn create_category(
    app: AppHandle,
    state: tauri::State<AppState>,
    name: String,
    color: String,
) -> CommandResult<Category> {
    let res = state
        .repo
        .create_category(&name, &color, Utc::now().timestamp_millis())
        .map_err(err);
    if res.is_ok() {
        let _ = app.emit("categories-changed", ());
        let _ = app.emit("clips-changed", ());
    }
    res
}

#[tauri::command]
fn update_category(
    app: AppHandle,
    state: tauri::State<AppState>,
    id: String,
    name: String,
    color: String,
) -> CommandResult<()> {
    let res = state.repo.update_category(&id, &name, &color).map_err(err);
    if res.is_ok() {
        let _ = app.emit("categories-changed", ());
    }
    res
}

#[tauri::command]
fn delete_category(app: AppHandle, state: tauri::State<AppState>, id: String) -> CommandResult<()> {
    let res = state.repo.delete_category(&id).map_err(err);
    if res.is_ok() {
        let _ = app.emit("categories-changed", ());
        let _ = app.emit("clips-changed", ());
    }
    res
}

#[tauri::command]
fn assign_category(
    app: AppHandle,
    state: tauri::State<AppState>,
    clip_id: String,
    category_id: String,
) -> CommandResult<()> {
    let res = state
        .repo
        .assign_category(&clip_id, &category_id, Utc::now().timestamp_millis())
        .map_err(err);
    if res.is_ok() {
        let _ = app.emit("clips-changed", ());
    }
    res
}

#[tauri::command]
fn unassign_category(
    app: AppHandle,
    state: tauri::State<AppState>,
    clip_id: String,
    category_id: String,
) -> CommandResult<()> {
    let res = state
        .repo
        .unassign_category(&clip_id, &category_id)
        .map_err(err);
    if res.is_ok() {
        let _ = app.emit("clips-changed", ());
    }
    res
}

/// Save settings with full rollback on partial failure.
///
/// Rollback order:
/// 1. Validate new settings.
/// 2. Register new shortcut (if changed).
///    - On fail → return Err, old shortcut still active.
/// 3. Unregister old shortcut.
/// 4. Apply autostart (if changed).
///    - On fail → re-register old shortcut, return Err.
/// 5. Write settings file atomically.
///    - On fail → rollback autostart, re-register old shortcut, return Err.
/// 6. Update runtime state (paused flag, tray, etc.).
/// 7. Emit settings-changed.
#[tauri::command]
fn save_settings(
    app: AppHandle,
    state: tauri::State<AppState>,
    settings: Settings,
) -> CommandResult<()> {
    // Step 1: validate.
    settings.validate().map_err(err)?;

    let previous = state.settings.get();
    let shortcut_changed = previous.shortcut != settings.shortcut;
    let autostart_changed = previous.autostart != settings.autostart;
    let old_registered = state.registered_shortcut.lock().clone();

    // Step 2 & 3: register new shortcut and unregister old shortcut if registered.
    if shortcut_changed {
        match &old_registered {
            Some(old_key) => {
                if let Err(e) = app.global_shortcut().register(settings.shortcut.as_str()) {
                    return Err(err(format!(
                        "Не удалось зарегистрировать горячую клавишу '{}': {e}",
                        settings.shortcut
                    )));
                }
                if let Err(e) = app.global_shortcut().unregister(old_key.as_str()) {
                    log::error!(
                        "save_settings: could not unregister old shortcut '{old_key}': {e}"
                    );
                    let _ = app.global_shortcut().unregister(settings.shortcut.as_str());
                    *state.registered_shortcut.lock() = Some(old_key.clone());
                    return Err(err(format!(
                        "Не удалось снять старую горячую клавишу '{old_key}': {e}"
                    )));
                }
                *state.registered_shortcut.lock() = Some(settings.shortcut.clone());
            }
            None => {
                if let Err(e) = app.global_shortcut().register(settings.shortcut.as_str()) {
                    return Err(err(format!(
                        "Не удалось зарегистрировать горячую клавишу '{}': {e}",
                        settings.shortcut
                    )));
                }
                *state.registered_shortcut.lock() = Some(settings.shortcut.clone());
            }
        }
    }

    // Step 4: apply autostart.
    if autostart_changed {
        if let Err(e) = settings::set_autostart(settings.autostart) {
            // Rollback shortcut.
            if shortcut_changed {
                let _ = app.global_shortcut().unregister(settings.shortcut.as_str());
                if let Some(old_key) = &old_registered {
                    if app.global_shortcut().register(old_key.as_str()).is_ok() {
                        *state.registered_shortcut.lock() = Some(old_key.clone());
                    } else {
                        *state.registered_shortcut.lock() = None;
                    }
                } else {
                    *state.registered_shortcut.lock() = None;
                }
            }
            return Err(err(format!("Не удалось изменить автозапуск: {e}")));
        }
    }

    // Step 5: write settings file atomically.
    if let Err(e) = state.settings.save(settings.clone()) {
        // Rollback autostart.
        if autostart_changed {
            let _ = settings::set_autostart(previous.autostart);
        }
        // Rollback shortcut.
        if shortcut_changed {
            let _ = app.global_shortcut().unregister(settings.shortcut.as_str());
            if let Some(old_key) = &old_registered {
                if app.global_shortcut().register(old_key.as_str()).is_ok() {
                    *state.registered_shortcut.lock() = Some(old_key.clone());
                } else {
                    *state.registered_shortcut.lock() = None;
                }
            } else {
                *state.registered_shortcut.lock() = None;
            }
        }
        return Err(err(format!("Не удалось сохранить настройки: {e}")));
    }

    // Step 6: update runtime state.
    state.paused.store(settings.paused, Ordering::Relaxed);
    update_pause_indicators(&app, settings.paused);

    // Step 7: notify.
    let _ = app.emit("settings-changed", ());
    Ok(())
}

#[tauri::command]
fn get_integration_status(
    state: tauri::State<AppState>,
) -> CommandResult<diagnostics::IntegrationStatus> {
    let project = settings::project_dirs().map_err(err)?;
    let data_dir = project.data_dir();
    let shortcut_registered = state.registered_shortcut.lock().is_some();
    let autostart_enabled = settings::is_autostart_actual_enabled();
    Ok(diagnostics::get_integration_status(
        data_dir,
        shortcut_registered,
        autostart_enabled,
    ))
}

#[tauri::command]
fn configure_extension_id(
    state: tauri::State<AppState>,
    extension_id: String,
) -> CommandResult<diagnostics::IntegrationStatus> {
    diagnostics::save_user_extension_manifest(&extension_id).map_err(err)?;
    let project = settings::project_dirs().map_err(err)?;
    let data_dir = project.data_dir();
    let shortcut_registered = state.registered_shortcut.lock().is_some();
    let autostart_enabled = settings::is_autostart_actual_enabled();
    Ok(diagnostics::get_integration_status(
        data_dir,
        shortcut_registered,
        autostart_enabled,
    ))
}

#[tauri::command]
fn open_extension_dir() -> CommandResult<String> {
    let candidates = [
        PathBuf::from(diagnostics::SYSTEM_CHROME_EXTENSION_DIR),
        PathBuf::from("chrome-extension"),
        PathBuf::from("../chrome-extension"),
    ];
    let mut target_dir = None;
    for candidate in &candidates {
        if candidate.exists() {
            target_dir = Some(
                candidate
                    .canonicalize()
                    .unwrap_or_else(|_| candidate.clone()),
            );
            break;
        }
    }
    let path = target_dir.ok_or_else(|| "Каталог Chrome-расширения не найден.".to_string())?;
    let path_str = path.to_string_lossy().to_string();
    std::process::Command::new("xdg-open")
        .arg(&path_str)
        .spawn()
        .map_err(|e| format!("Не удалось открыть каталог: {e}"))?;
    Ok(path_str)
}

#[tauri::command]
fn open_chrome_extensions_page() -> CommandResult<()> {
    let binaries = [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "xdg-open",
    ];
    for bin in &binaries {
        if std::process::Command::new(bin)
            .arg("chrome://extensions")
            .spawn()
            .is_ok()
        {
            return Ok(());
        }
    }
    Err("Не удалось открыть страницу расширений (браузер не найден).".into())
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn toggle_popup(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("popup") {
        if w.is_visible().unwrap_or(false) {
            let _ = w.hide();
        } else {
            let _ = app.emit("clips-changed", ());
            let _ = app.emit("categories-changed", ());
            let _ = app.emit("settings-changed", ());
            let _ = w.center();
            let _ = w.show();
            let _ = w.set_focus();
        }
    }
}

fn update_pause_indicators(app: &AppHandle, paused: bool) {
    if let Some(tray) = app.tray_by_id("kitsupin") {
        let tooltip = if paused {
            "KitsuPin — запись приостановлена"
        } else {
            "KitsuPin — история буфера обмена"
        };
        let _ = tray.set_tooltip(Some(tooltip));
    }
    if let Some(item) = app.state::<AppState>().pause_item.lock().as_ref() {
        let label = if paused {
            "Возобновить запись истории"
        } else {
            "Приостановить запись истории"
        };
        let _ = item.set_text(label);
    }
}

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Открыть KitsuPin", true, None::<&str>)?;
    let pause_label = if app.state::<AppState>().paused.load(Ordering::Relaxed) {
        "Возобновить запись истории"
    } else {
        "Приостановить запись истории"
    };
    let pause = MenuItem::with_id(app, "pause", pause_label, true, None::<&str>)?;
    let clear = MenuItem::with_id(
        app,
        "clear",
        "Очистить незакреплённую историю",
        true,
        None::<&str>,
    )?;
    let settings = MenuItem::with_id(app, "settings", "Настройки", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &pause, &clear, &settings, &quit])?;
    *app.state::<AppState>().pause_item.lock() = Some(pause.clone());
    let initial_tooltip = if app.state::<AppState>().paused.load(Ordering::Relaxed) {
        "KitsuPin — запись приостановлена"
    } else {
        "KitsuPin — история буфера обмена"
    };
    TrayIconBuilder::with_id("kitsupin")
        .icon(app.default_window_icon().expect("app icon").clone())
        .tooltip(initial_tooltip)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_popup(tray.app_handle())
            }
        })
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "settings" => {
                show_main(app);
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.emit("open-settings", ());
                }
            }
            "pause" => {
                let s = app.state::<AppState>();
                let mut value = s.settings.get();
                value.paused = !s.paused.load(Ordering::Relaxed);
                if let Err(e) = save_settings(app.clone(), s, value) {
                    log::error!("Не удалось изменить режим паузы из tray: {e}");
                }
            }
            "clear" => {
                show_main(app);
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.emit("confirm-clear-history", ());
                }
            }
            "quit" => {
                app.state::<AppState>()
                    .quitting
                    .store(true, Ordering::Relaxed);
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

// ── Single-instance: advisory file lock ──────────────────────────────────────

fn instance_lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join("kitsupin.lock")
}

fn single_instance_socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("app.sock")
}

/// Acquire an exclusive advisory file lock for single-instance enforcement.
///
/// Returns the open File that must be kept alive for the duration of the process.
/// If the lock is already held by another process, returns None.
fn try_acquire_instance_lock(data_dir: &Path) -> std::io::Result<Option<File>> {
    use fs2::FileExt;
    use std::os::unix::fs::OpenOptionsExt;

    let lock_path = instance_lock_path(data_dir);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&lock_path)?;

    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(file)),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(e) => Err(e),
    }
}

/// Try to signal the existing instance to show its main window.
/// Retries up to `retries` times with `delay_ms` between attempts.
fn try_signal_existing(socket_path: &Path, retries: u32, delay_ms: u64) -> bool {
    for attempt in 0..=retries {
        if let Ok(mut stream) = UnixStream::connect(socket_path) {
            let _ = stream.write_all(b"show_main\n");
            let _ = stream.flush();
            return true;
        }
        if attempt < retries {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
    }
    false
}

fn start_single_instance_listener(data_dir: &Path, app: AppHandle) {
    let socket_path = single_instance_socket_path(data_dir);
    // Remove stale socket from previous crash (lock is already held so we know we're primary).
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }
    match UnixListener::bind(&socket_path) {
        Ok(listener) => {
            if let Err(e) = std::fs::set_permissions(
                &socket_path,
                std::os::unix::fs::PermissionsExt::from_mode(0o600),
            ) {
                log::error!("Single-instance listener: не удалось установить права 0600 на socket: {e}");
            }
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_ok() && line.trim() == "show_main" {
                        show_main(&app);
                    }
                    // Unknown commands are silently ignored for security.
                }
            });
        }
        Err(e) => {
            log::error!("Single-instance listener: не удалось создать socket: {e}");
        }
    }
}

pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("KitsuPin — Локальная история буфера обмена для KDE X11");
        println!();
        println!("Использование:");
        println!("  kitsupin [ОПЦИИ]");
        println!();
        println!("Опции:");
        println!("  --version    Показать версию приложения и выйти");
        println!("  --diagnose   Запустить без GUI и проверить состояние интеграции");
        println!("  --background Запустить приложения в свёрнутом режиме (в системном трее)");
        println!("  --help, -h   Показать эту справку");
        return;
    }
    if args.iter().any(|arg| arg == "--version" || arg == "-v") {
        println!("KitsuPin {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|arg| arg == "--diagnose") {
        let project = match settings::project_dirs() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Ошибка при получении каталогов XDG: {e}");
                std::process::exit(1);
            }
        };
        let data_dir = project.data_dir();
        let autostart_actual = settings::is_autostart_actual_enabled();
        let status = diagnostics::get_integration_status(data_dir, true, autostart_actual);
        println!("=== KitsuPin Integration Diagnostics ===");
        println!("OS Linux: {}", status.is_linux);
        println!(
            "DE: {}",
            status
                .desktop_environment
                .as_deref()
                .unwrap_or("неизвестно")
        );
        println!(
            "Session: {}",
            status.session_type.as_deref().unwrap_or("неизвестно")
        );
        println!("X11 Supported: {}", status.is_supported_x11);
        println!("Chrome Detected: {}", status.chrome_detected);
        println!(
            "Native Host Binary Exists: {}",
            status.native_host_binary_exists
        );
        println!("Native Host Executable: {}", status.native_host_executable);
        println!("Native Manifest Exists: {}", status.native_manifest_exists);
        println!("Native Manifest Valid: {}", status.native_manifest_valid);
        println!(
            "Extension ID: {}",
            status.extension_id.as_deref().unwrap_or("не задан")
        );
        println!(
            "Native Socket Available: {}",
            status.native_socket_available
        );
        println!("Problems Count: {}", status.problems.len());
        for p in &status.problems {
            println!(
                "  [{}] {}: {}",
                p.severity.to_uppercase(),
                p.title,
                p.description
            );
        }
        let has_critical = status.problems.iter().any(|p| p.severity == "error");
        if has_critical {
            eprintln!("Диагностика завершилась с критическими ошибками.");
            std::process::exit(1);
        } else {
            println!("Диагностика прошла успешно (или с предупреждениями).");
            std::process::exit(0);
        }
    }

    let background = std::env::args().any(|value| value == "--background");
    let project = settings::project_dirs().expect("XDG directories");
    let data_dir = project.data_dir().to_path_buf();
    // ── Legacy migration (BEFORE creating target directory or locking single-instance) ──
    if migration::migrate_pastily_to_kitsupin()
        == migration::LegacyMigrationResult::ConflictPreserved
    {
        log::error!("KitsuPin startup aborted: migration lock active or migration conflict.");
        return;
    }

    std::fs::create_dir_all(&data_dir).expect("data directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700))
            .expect("failed to set 0700 permissions on data directory");
    }

    // ── Atomic single-instance check (BEFORE DB open or starting threads) ──
    let _lock_file = match try_acquire_instance_lock(&data_dir) {
        Ok(Some(file)) => file,
        Ok(None) => {
            // Another instance holds the lock. Signal it and exit.
            log::info!("KitsuPin уже запущен. Показываем главное окно.");
            let socket_path = single_instance_socket_path(&data_dir);
            if !try_signal_existing(&socket_path, 3, 100) {
                log::warn!("Не удалось подключиться к socket существующего экземпляра.");
            }
            return;
        }
        Err(e) => {
            log::error!("Не удалось создать lock-файл single-instance: {e}. Завершение работы.");
            return;
        }
    };

    // ── Open DB and start application (lock is held) ───────────────────────
    let repo = Arc::new(Repository::open(&data_dir.join("kitsupin.sqlite3")).expect("database"));
    let settings = SettingsStore::load(&data_dir).expect("settings");
    let paused = Arc::new(AtomicBool::new(settings.get().paused));
    let guard = Arc::new(OwnCopyGuard::default());
    let clipboard = Arc::new(ClipboardAccess::default());
    let metadata = Arc::new(MetadataBuffer::default());

    let initial_shortcut = settings.get().shortcut.clone();

    let state = AppState {
        repo: repo.clone(),
        settings: settings.clone(),
        guard: guard.clone(),
        clipboard: clipboard.clone(),
        paused: paused.clone(),
        quitting: AtomicBool::new(false),
        pause_item: Mutex::new(None),
        registered_shortcut: Mutex::new(Some(initial_shortcut)),
    };

    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .max_file_size(2_000_000)
                .build(),
        )
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _, event| {
                    if event.state() == ShortcutState::Pressed {
                        toggle_popup(app)
                    }
                })
                .build(),
        )
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            consume_invalid_settings_warning,
            list_clips,
            get_clip_content,
            copy_clip,
            delete_clip,
            set_pinned,
            clear_unpinned,
            create_category,
            update_category,
            delete_category,
            assign_category,
            unassign_category,
            save_settings,
            get_integration_status,
            configure_extension_id,
            open_extension_dir,
            open_chrome_extensions_page
        ])
        .setup(move |app| {
            WebviewWindowBuilder::new(
                app,
                "popup",
                WebviewUrl::App("index.html?mode=popup".into()),
            )
            .title("KitsuPin")
            .inner_size(520.0, 620.0)
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .visible(false)
            .resizable(false)
            .build()?;

            if let Err(error) = setup_tray(app) {
                log::error!("KDE System Tray недоступен: {error}");
            }

            let shortcut = settings.get().shortcut;
            if let Err(e) = app.global_shortcut().register(shortcut.as_str()) {
                log::error!("Горячая клавиша недоступна: {e}");
                // Clear tracked shortcut so user can re-set it via settings.
                *app.state::<AppState>().registered_shortcut.lock() = None;
            }

            // Set up late-reconciliation callback: when Chrome metadata arrives via socket,
            // try to attach it to a recently saved clipboard entry.
            let repo_reconcile = repo.clone();
            let app_reconcile = app.handle().clone();
            let metadata_reconcile = metadata.clone();
            let reconcile_callback = Arc::new(move |_: browser_metadata::BrowserCopyEvent| {
                metadata_reconcile.reconcile_pending(
                    &repo_reconcile,
                    Some(&|| {
                        let _ = app_reconcile.emit("clips-changed", ());
                    }),
                );
            });

            if let Err(error) = browser_metadata::start_socket_server(
                browser_metadata::socket_path(&data_dir),
                metadata.clone(),
                reconcile_callback,
            ) {
                log::error!("Native Messaging socket недоступен: {error}");
            }

            start_single_instance_listener(&data_dir, app.handle().clone());

            clipboard::start(
                app.handle().clone(),
                repo.clone(),
                metadata.clone(),
                guard.clone(),
                clipboard.clone(),
                paused.clone(),
            );
            jobs::start(app.handle().clone(), repo.clone(), settings.clone());

            if settings.get().autostart {
                let _ = settings::set_autostart(true);
            }
            if !background {
                show_main(app.handle());
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("KitsuPin startup")
        .run(|app, event| match event {
            RunEvent::WindowEvent {
                label,
                event: tauri::WindowEvent::CloseRequested { api, .. },
                ..
            } if label == "main" => {
                api.prevent_close();
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }
            _ => {}
        });
}
