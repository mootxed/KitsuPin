use crate::{persistence::Repository, settings::SettingsStore};
use chrono::Utc;
use std::{sync::Arc, time::Duration};
use tauri::Emitter;

pub fn start(app: tauri::AppHandle, repo: Arc<Repository>, settings: Arc<SettingsStore>) {
    let clean = |app_handle: &tauri::AppHandle, repo: &Repository, settings: &SettingsStore| {
        let current_settings = settings.get();
        if !current_settings.is_valid() {
            log::error!(
                "Очистка пропущена: недопустимый срок хранения retention_days={}",
                current_settings.retention_days
            );
            return;
        }
        match repo.cleanup(
            current_settings.retention_days,
            Utc::now().timestamp_millis(),
        ) {
            Ok(n) if n > 0 => {
                log::info!("Удалено устаревших карточек: {n}");
                let _ = app_handle.emit("clips-changed", ());
            }
            Err(e) => log::error!("Ошибка очистки: {e}"),
            _ => {}
        }
    };
    clean(&app, &repo, &settings);
    let app_handle = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(86_400));
        clean(&app_handle, &repo, &settings);
    });
}
