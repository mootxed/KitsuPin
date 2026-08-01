use crate::{persistence::Repository, settings::SettingsStore};
use chrono::Utc;
use std::{sync::Arc, time::Duration};
pub fn start(repo: Arc<Repository>, settings: Arc<SettingsStore>) {
    fn clean(repo: &Repository, settings: &SettingsStore) {
        let current_settings = settings.get();
        if !current_settings.is_valid() {
            log::error!(
                "Очистка пропущена: недопустимый срок хранения retention_days={}",
                current_settings.retention_days
            );
            return;
        }
        match repo.cleanup(current_settings.retention_days, Utc::now().timestamp_millis()) {
            Ok(n) if n > 0 => log::info!("Удалено устаревших карточек: {n}"),
            Err(e) => log::error!("Ошибка очистки: {e}"),
            _ => {}
        }
    }
    clean(&repo, &settings);
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(86_400));
        clean(&repo, &settings)
    });
}
