pub mod browser_metadata;
mod clipboard;
mod domain;
mod jobs;
pub mod native_host;
mod persistence;
mod settings;

use browser_metadata::MetadataBuffer;
use chrono::Utc;
use clipboard::ClipboardAccess;
use domain::{Category, Clip, ClipQuery, OwnCopyGuard};
use parking_lot::Mutex;
use persistence::Repository;
use serde::Serialize;
use settings::{Settings, SettingsStore};
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
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Bootstrap {
    clips: Vec<Clip>,
    categories: Vec<Category>,
    settings: Settings,
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
    })
}
#[tauri::command]
fn list_clips(state: tauri::State<AppState>, query: ClipQuery) -> CommandResult<Vec<Clip>> {
    state.repo.list_clips(&query).map_err(err)
}
#[tauri::command]
fn copy_clip(
    app: AppHandle,
    state: tauri::State<AppState>,
    id: String,
    content: String,
    popup: bool,
) -> CommandResult<()> {
    clipboard::set_clipboard(&content, &state.guard, &state.clipboard).map_err(err)?;
    if popup {
        if let Some(w) = app.get_webview_window("popup") {
            let _ = w.hide();
        }
    }
    let _ = id;
    Ok(())
}
#[tauri::command]
fn delete_clip(state: tauri::State<AppState>, id: String) -> CommandResult<()> {
    state.repo.delete_clip(&id).map_err(err)
}
#[tauri::command]
fn set_pinned(state: tauri::State<AppState>, id: String, pinned: bool) -> CommandResult<()> {
    state.repo.set_pinned(&id, pinned).map_err(err)
}
#[tauri::command]
fn clear_unpinned(state: tauri::State<AppState>) -> CommandResult<usize> {
    state.repo.clear_unpinned().map_err(err)
}
#[tauri::command]
fn create_category(
    state: tauri::State<AppState>,
    name: String,
    color: String,
) -> CommandResult<Category> {
    state
        .repo
        .create_category(&name, &color, &Utc::now().to_rfc3339())
        .map_err(err)
}
#[tauri::command]
fn update_category(
    state: tauri::State<AppState>,
    id: String,
    name: String,
    color: String,
) -> CommandResult<()> {
    state.repo.update_category(&id, &name, &color).map_err(err)
}
#[tauri::command]
fn delete_category(state: tauri::State<AppState>, id: String) -> CommandResult<()> {
    state.repo.delete_category(&id).map_err(err)
}
#[tauri::command]
fn assign_category(
    state: tauri::State<AppState>,
    clip_id: String,
    category_id: String,
) -> CommandResult<()> {
    state
        .repo
        .assign_category(&clip_id, &category_id, &Utc::now().to_rfc3339())
        .map_err(err)
}
#[tauri::command]
fn unassign_category(
    state: tauri::State<AppState>,
    clip_id: String,
    category_id: String,
) -> CommandResult<()> {
    state
        .repo
        .unassign_category(&clip_id, &category_id)
        .map_err(err)
}
#[tauri::command]
fn save_settings(
    app: AppHandle,
    state: tauri::State<AppState>,
    settings: Settings,
) -> CommandResult<()> {
    let previous = state.settings.get();
    if previous.shortcut != settings.shortcut {
        app.global_shortcut()
            .unregister(previous.shortcut.as_str())
            .map_err(err)?;
        if let Err(error) = app.global_shortcut().register(settings.shortcut.as_str()) {
            let _ = app.global_shortcut().register(previous.shortcut.as_str());
            return Err(err(error));
        }
    }
    settings::set_autostart(settings.autostart).map_err(err)?;
    state.paused.store(settings.paused, Ordering::Relaxed);
    update_pause_indicators(&app, settings.paused);
    state.settings.save(settings).map_err(err)
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
            let _ = w.center();
            let _ = w.show();
            let _ = w.set_focus();
        }
    }
}

fn update_pause_indicators(app: &AppHandle, paused: bool) {
    if let Some(tray) = app.tray_by_id("pastily") {
        let tooltip = if paused {
            "Pastily — запись приостановлена"
        } else {
            "Pastily — история буфера обмена"
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
    let open = MenuItem::with_id(app, "open", "Открыть Pastily", true, None::<&str>)?;
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
        "Pastily — запись приостановлена"
    } else {
        "Pastily — история буфера обмена"
    };
    TrayIconBuilder::with_id("pastily")
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
                let _ = app.emit("open-settings", ());
            }
            "pause" => {
                let s = app.state::<AppState>();
                let next = !s.paused.load(Ordering::Relaxed);
                s.paused.store(next, Ordering::Relaxed);
                let mut value = s.settings.get();
                value.paused = next;
                let _ = s.settings.save(value);
                update_pause_indicators(app, next);
            }
            "clear" => {
                let _ = app.state::<AppState>().repo.clear_unpinned();
                let _ = app.emit("clips-changed", ());
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

pub fn run() {
    let background = std::env::args().any(|value| value == "--background");
    let project = settings::project_dirs().expect("XDG directories");
    let data_dir = project.data_dir().to_path_buf();
    std::fs::create_dir_all(&data_dir).expect("data directory");
    let repo = Arc::new(Repository::open(&data_dir.join("pastily.sqlite3")).expect("database"));
    let settings = SettingsStore::load(&data_dir).expect("settings");
    let paused = Arc::new(AtomicBool::new(settings.get().paused));
    let guard = Arc::new(OwnCopyGuard::default());
    let clipboard = Arc::new(ClipboardAccess::default());
    let metadata = Arc::new(MetadataBuffer::default());
    let state = AppState {
        repo: repo.clone(),
        settings: settings.clone(),
        guard: guard.clone(),
        clipboard: clipboard.clone(),
        paused: paused.clone(),
        quitting: AtomicBool::new(false),
        pause_item: Mutex::new(None),
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
            list_clips,
            copy_clip,
            delete_clip,
            set_pinned,
            clear_unpinned,
            create_category,
            update_category,
            delete_category,
            assign_category,
            unassign_category,
            save_settings
        ])
        .setup(move |app| {
            WebviewWindowBuilder::new(
                app,
                "popup",
                WebviewUrl::App("index.html?mode=popup".into()),
            )
            .title("Pastily")
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
            }
            if let Err(error) = browser_metadata::start_socket_server(
                browser_metadata::socket_path(&data_dir),
                metadata.clone(),
            ) {
                log::error!("Native Messaging socket недоступен: {error}");
            }
            clipboard::start(
                app.handle().clone(),
                repo.clone(),
                metadata.clone(),
                guard.clone(),
                clipboard.clone(),
                paused.clone(),
            );
            jobs::start(repo.clone(), settings.clone());
            if settings.get().autostart {
                let _ = settings::set_autostart(true);
            }
            if !background {
                show_main(app.handle());
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Pastily startup")
        .run(|app, event| match event {
            RunEvent::ExitRequested { api, .. }
                if !app.state::<AppState>().quitting.load(Ordering::Relaxed) =>
            {
                api.prevent_exit()
            }
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
