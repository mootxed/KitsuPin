use crate::{persistence::Repository, settings::SettingsStore};
use chrono::Utc;
use std::{sync::Arc, time::Duration};
pub fn start(repo: Arc<Repository>, settings: Arc<SettingsStore>) {
    fn clean(repo: &Repository, settings: &SettingsStore) {
        match repo.cleanup(settings.get().retention_days, &Utc::now().to_rfc3339()) {
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
