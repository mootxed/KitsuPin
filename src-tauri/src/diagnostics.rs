use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationStatus {
    pub is_linux: bool,
    pub desktop_environment: Option<String>,
    pub session_type: Option<String>,
    pub is_supported_x11: bool,
    pub chrome_detected: bool,
    pub extension_id: Option<String>,
    pub native_host_binary_exists: bool,
    pub native_host_executable: bool,
    pub native_manifest_exists: bool,
    pub native_manifest_valid: bool,
    pub chrome_manifest_valid: bool,
    pub chromium_manifest_valid: bool,
    pub native_socket_available: bool,
    pub native_messaging_configured: bool,
    pub native_messaging_connected: bool,
    pub last_native_message_at: Option<i64>,
    pub shortcut_registered: Option<bool>,
    pub autostart_enabled: bool,
    pub problems: Vec<IntegrationProblem>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationProblem {
    pub id: String,
    pub severity: String, // "error" | "warning" | "info"
    pub title: String,
    pub description: String,
    pub action: Option<String>,
}

pub const NATIVE_HOST_NAME: &str = "io.github.mootxed.kitsupin.native";
pub const SYSTEM_LIB_DIR: &str = "/usr/lib/kitsupin";
pub const SYSTEM_NATIVE_HOST_PATH: &str = "/usr/lib/kitsupin/kitsupin-native-host";
pub const SYSTEM_CHROME_EXTENSION_DIR: &str = "/usr/lib/kitsupin/resources/chrome-extension";
pub const SYSTEM_UNINSTALL_SCRIPT_PATH: &str =
    "/usr/lib/kitsupin/resources/scripts/uninstall-user-data.sh";
pub const SYSTEM_MANIFEST_PATHS: &[&str] = &[
    "/etc/opt/chrome/native-messaging-hosts/io.github.mootxed.kitsupin.native.json",
    "/etc/chromium/native-messaging-hosts/io.github.mootxed.kitsupin.native.json",
];
pub const USER_MANIFEST_DIRS: &[&str] = &[
    "google-chrome/NativeMessagingHosts",
    "chromium/NativeMessagingHosts",
];

/// Validates that an extension ID consists of exactly 32 characters in the range 'a'..='p'.
pub fn validate_extension_id(id: &str) -> bool {
    id.len() == 32 && id.chars().all(|c| ('a'..='p').contains(&c))
}

/// Generates a Native Messaging Manifest JSON for Google Chrome.
pub fn generate_native_manifest(host_path: &Path, extension_id: &str) -> Result<String, String> {
    if !validate_extension_id(extension_id) {
        return Err(format!(
            "Неверный ID Chrome-расширения: '{extension_id}'. Должно быть 32 символа a-p."
        ));
    }
    let manifest = serde_json::json!({
        "name": NATIVE_HOST_NAME,
        "description": "KitsuPin Native Messaging Host",
        "path": host_path.to_string_lossy(),
        "type": "stdio",
        "allowed_origins": [
            format!("chrome-extension://{extension_id}/")
        ]
    });
    serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())
}

/// Parses and validates a Native Messaging manifest.
/// Returns extracted extension_id if valid.
pub fn validate_manifest_content(content: &str) -> Result<String, String> {
    let val: serde_json::Value =
        serde_json::from_str(content).map_err(|e| format!("Некорректный JSON в манифесте: {e}"))?;

    let name = val.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if name != NATIVE_HOST_NAME {
        return Err(format!(
            "Имя манифеста '{name}' не совпадает с '{NATIVE_HOST_NAME}'"
        ));
    }

    let manifest_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if manifest_type != "stdio" {
        return Err(format!(
            "Тип манифеста '{manifest_type}' должен быть 'stdio'"
        ));
    }

    let path_str = val.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if path_str.is_empty() {
        return Err("Манифест не содержит пути path".into());
    }
    let target_path = Path::new(path_str);
    if !target_path.exists() {
        return Err(format!(
            "Указанный в манифесте путь '{path_str}' не существует"
        ));
    }
    if !is_file_executable(target_path) {
        return Err(format!(
            "Указанный в манифесте файл '{path_str}' не является исполняемым"
        ));
    }

    let origins = val.get("allowed_origins").and_then(|v| v.as_array());
    if let Some(origins_arr) = origins {
        for origin in origins_arr {
            if let Some(s) = origin.as_str() {
                if let Some(stripped) = s.strip_prefix("chrome-extension://") {
                    if let Some(id) = stripped.strip_suffix('/') {
                        if validate_extension_id(id) {
                            return Ok(id.to_string());
                        }
                    }
                }
            }
        }
    }
    Err("Не найден валидный ID расширения в allowed_origins".into())
}

pub fn is_file_executable(path: &Path) -> bool {
    if let Ok(metadata) = fs::metadata(path) {
        if !metadata.is_file() {
            return false;
        }
        let mode = metadata.permissions().mode();
        (mode & 0o111) != 0
    } else {
        false
    }
}

pub fn check_chrome_installed() -> bool {
    let candidates = [
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
    ];
    for path in &candidates {
        if Path::new(path).exists() {
            return true;
        }
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for bin in &[
                "google-chrome",
                "google-chrome-stable",
                "chromium",
                "chromium-browser",
            ] {
                if dir.join(bin).exists() {
                    return true;
                }
            }
        }
    }
    false
}

pub fn get_user_manifest_paths() -> Vec<PathBuf> {
    let config_dir = match std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    {
        Some(dir) => dir,
        None => return Vec::new(),
    };

    USER_MANIFEST_DIRS
        .iter()
        .map(|dir| {
            config_dir
                .join(dir)
                .join(format!("{NATIVE_HOST_NAME}.json"))
        })
        .collect()
}

pub fn get_user_manifest_path() -> Option<PathBuf> {
    get_user_manifest_paths().into_iter().next()
}

pub fn resolve_native_host_path() -> Option<PathBuf> {
    let system_bin = Path::new(SYSTEM_NATIVE_HOST_PATH);
    if system_bin.exists() {
        return Some(system_bin.to_path_buf());
    }
    if let Ok(env_path) = std::env::var("KITSUPIN_NATIVE_HOST") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let local_bin = dir.join("kitsupin-native-host");
            if local_bin.exists() {
                return Some(local_bin);
            }
        }
    }
    None
}

pub fn get_integration_status(
    data_dir: &Path,
    shortcut_registered: Option<bool>,
    autostart_enabled: bool,
) -> IntegrationStatus {
    let is_linux = cfg!(target_os = "linux");
    let desktop_environment = std::env::var("XDG_CURRENT_DESKTOP").ok();
    let session_type = std::env::var("XDG_SESSION_TYPE").ok();

    let is_supported_x11 = match session_type.as_deref() {
        Some(st) => st.eq_ignore_ascii_case("x11"),
        None => std::env::var("DISPLAY").is_ok(),
    };

    let chrome_detected = check_chrome_installed();
    let native_host_path = resolve_native_host_path();
    let native_host_binary_exists = native_host_path.is_some();
    let native_host_executable = native_host_path
        .as_ref()
        .map(|p| is_file_executable(p))
        .unwrap_or(false);

    let mut native_manifest_exists = false;
    let mut chrome_manifest_valid = false;
    let mut chromium_manifest_valid = false;
    let mut extension_id: Option<String> = None;

    let user_manifest_paths = get_user_manifest_paths();

    let mut check_path = |path: &Path| -> bool {
        if path.exists() {
            native_manifest_exists = true;
            if let Ok(content) = fs::read_to_string(path) {
                if let Ok(ext_id) = validate_manifest_content(&content) {
                    if extension_id.is_none() {
                        extension_id = Some(ext_id);
                    }
                    return true;
                }
            }
        }
        false
    };

    if !SYSTEM_MANIFEST_PATHS.is_empty() && check_path(Path::new(SYSTEM_MANIFEST_PATHS[0])) {
        chrome_manifest_valid = true;
    }
    if SYSTEM_MANIFEST_PATHS.len() > 1 && check_path(Path::new(SYSTEM_MANIFEST_PATHS[1])) {
        chromium_manifest_valid = true;
    }

    if !user_manifest_paths.is_empty()
        && !chrome_manifest_valid
        && check_path(&user_manifest_paths[0])
    {
        chrome_manifest_valid = true;
    }
    if user_manifest_paths.len() > 1
        && !chromium_manifest_valid
        && check_path(&user_manifest_paths[1])
    {
        chromium_manifest_valid = true;
    }

    let native_manifest_valid = chrome_manifest_valid || chromium_manifest_valid;
    let socket_path = crate::browser_metadata::socket_path(data_dir);
    let native_socket_available = socket_path.exists();
    let last_native_message_at = crate::browser_metadata::get_last_message_at();
    let native_messaging_configured = native_socket_available && native_manifest_valid;
    let native_messaging_connected =
        native_messaging_configured && last_native_message_at.is_some();

    let mut problems = Vec::new();

    if !is_linux {
        problems.push(IntegrationProblem {
            id: "os_not_linux".into(),
            severity: "error".into(),
            title: "Операционная система не Linux".into(),
            description: "KitsuPin официально поддерживается только в Linux (Ubuntu 24.04).".into(),
            action: None,
        });
    }

    if !is_supported_x11 {
        problems.push(IntegrationProblem {
            id: "wayland_session".into(),
            severity: "warning".into(),
            title: "Сеанс Wayland обнаружен".into(),
            description: "В сеансе Wayland глобальные горячие клавиши и доступ к буферу обмена X11 могут работать с ограничениями. Рекомендуется сеанс X11.".into(),
            action: None,
        });
    }

    if !chrome_detected {
        problems.push(IntegrationProblem {
            id: "chrome_missing".into(),
            severity: "warning".into(),
            title: "Google Chrome не найден".into(),
            description: "Установите Google Chrome для автоматической привязки заголовков страниц и доменов к истории буфера.".into(),
            action: None,
        });
    }

    if !native_host_binary_exists {
        problems.push(IntegrationProblem {
            id: "native_host_missing".into(),
            severity: "error".into(),
            title: "Native Host не установлен".into(),
            description: format!(
                "Файл бинарного хоста не найден по пути {SYSTEM_NATIVE_HOST_PATH}."
            ),
            action: None,
        });
    } else if !native_host_executable {
        problems.push(IntegrationProblem {
            id: "native_host_not_executable".into(),
            severity: "error".into(),
            title: "Нет прав на исполнение Native Host".into(),
            description:
                "Бинарный файл kitsupin-native-host не имеет флага исполняемого файла (+x).".into(),
            action: None,
        });
    }

    if !native_manifest_exists {
        problems.push(IntegrationProblem {
            id: "manifest_missing".into(),
            severity: "warning".into(),
            title: "Native Messaging manifest отсутствует".into(),
            description: "Chrome-расширение не сможет подключиться к KitsuPin. Введите ID расширения или используйте production .deb пакет.".into(),
            action: Some("configure_id".into()),
        });
    } else if !native_manifest_valid {
        problems.push(IntegrationProblem {
            id: "manifest_invalid".into(),
            severity: "error".into(),
            title: "Некорректный Native Messaging manifest".into(),
            description:
                "Манифест найден, но содержит недействительный ID расширения или путь к хосту."
                    .into(),
            action: Some("configure_id".into()),
        });
    }

    if !native_socket_available {
        problems.push(IntegrationProblem {
            id: "socket_unavailable".into(),
            severity: "error".into(),
            title: "Сокет приложения недоступен".into(),
            description: "Фоновый поток чтения метаданных браузера не смог открыть UNIX socket."
                .into(),
            action: None,
        });
    }

    if let Some(false) = shortcut_registered {
        problems.push(IntegrationProblem {
            id: "shortcut_conflict".into(),
            severity: "warning".into(),
            title: "Горячая клавиша не зарегистрирована".into(),
            description: "Глобальная сочетание клавиш занято другим приложением или не поддерживается окружением.".into(),
            action: Some("open_shortcut_settings".into()),
        });
    }

    if !autostart_enabled {
        problems.push(IntegrationProblem {
            id: "autostart_disabled".into(),
            severity: "info".into(),
            title: "Автозапуск отключён".into(),
            description: "KitsuPin не будет запускаться автоматически при входе в систему.".into(),
            action: Some("enable_autostart".into()),
        });
    }

    IntegrationStatus {
        is_linux,
        desktop_environment,
        session_type,
        is_supported_x11,
        chrome_detected,
        extension_id,
        native_host_binary_exists,
        native_host_executable,
        native_manifest_exists,
        native_manifest_valid,
        chrome_manifest_valid,
        chromium_manifest_valid,
        native_socket_available,
        native_messaging_configured,
        native_messaging_connected,
        last_native_message_at,
        shortcut_registered,
        autostart_enabled,
        problems,
    }
}

pub fn save_user_extension_manifest(extension_id: &str) -> Result<PathBuf, String> {
    if !validate_extension_id(extension_id) {
        return Err(format!(
            "Неверный ID расширения: '{extension_id}'. Должен состоять из 32 символов a-p."
        ));
    }
    let host_path =
        resolve_native_host_path().unwrap_or_else(|| PathBuf::from(SYSTEM_NATIVE_HOST_PATH));

    if !host_path.exists() {
        return Err(format!(
            "Файл хоста '{host_path:?}' не найден. Сначала установите KitsuPin."
        ));
    }

    let manifest_paths = get_user_manifest_paths();
    if manifest_paths.is_empty() {
        return Err("Не удалось определить домашний каталог пользователя".to_string());
    }

    let json_content = generate_native_manifest(&host_path, extension_id)?;

    let mut written_paths = Vec::new();
    let mut last_err = None;

    for manifest_path in &manifest_paths {
        if let Some(parent) = manifest_path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                last_err = Some(format!("Не удалось создать каталог {parent:?}: {e}"));
                continue;
            }
        }
        match fs::write(manifest_path, &json_content) {
            Ok(_) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Err(e) =
                        fs::set_permissions(manifest_path, fs::Permissions::from_mode(0o600))
                    {
                        last_err = Some(format!(
                            "Не удалось установить права на {manifest_path:?}: {e}"
                        ));
                        continue;
                    }
                }
                written_paths.push(manifest_path.clone());
            }
            Err(e) => {
                last_err = Some(format!(
                    "Не удалось записать манифест {manifest_path:?}: {e}"
                ));
            }
        }
    }

    if written_paths.is_empty() {
        return Err(
            last_err.unwrap_or_else(|| "Не удалось записать ни один manifest-файл".to_string())
        );
    }

    Ok(written_paths[0].clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_validate_extension_id() {
        assert!(validate_extension_id("abcdefghijklmnopabcdefghijklmnop"));
        assert!(!validate_extension_id("abcdefghijklmnopabcdefghijklmnoQ")); // capital Q
        assert!(!validate_extension_id("abcdefghijklmnop")); // 16 chars
        assert!(!validate_extension_id("abcdefghijklmnopabcdefghijklmno1")); // number 1
    }

    #[test]
    fn test_generate_native_manifest() {
        let path = Path::new(SYSTEM_NATIVE_HOST_PATH);
        let id = "abcdefghijklmnopabcdefghijklmnop";
        let res = generate_native_manifest(path, id).unwrap();
        assert!(res.contains(NATIVE_HOST_NAME));
        assert!(res.contains(SYSTEM_NATIVE_HOST_PATH));
        assert!(res.contains("chrome-extension://abcdefghijklmnopabcdefghijklmnop/"));

        let err = generate_native_manifest(path, "invalid-id");
        assert!(err.is_err());
    }

    #[test]
    fn test_validate_manifest_content() {
        let dir = tempdir().unwrap();
        let bin_path = dir.path().join("kitsupin-native-host");
        fs::write(&bin_path, "dummy").unwrap();
        fs::set_permissions(&bin_path, fs::Permissions::from_mode(0o755)).unwrap();

        let id = "abcdefghijklmnopabcdefghijklmnop";
        let valid_json = format!(
            r#"{{
            "name": "io.github.mootxed.kitsupin.native",
            "description": "test",
            "path": "{}",
            "type": "stdio",
            "allowed_origins": ["chrome-extension://{id}/"]
        }}"#,
            bin_path.to_string_lossy()
        );
        let parsed_id = validate_manifest_content(&valid_json).unwrap();
        assert_eq!(parsed_id, id);

        let invalid_name = valid_json.replace("io.github.mootxed.kitsupin.native", "wrong.name");
        assert!(validate_manifest_content(&invalid_name).is_err());

        let invalid_type = valid_json.replace("\"type\": \"stdio\"", "\"type\": \"invalid\"");
        assert!(validate_manifest_content(&invalid_type).is_err());

        let invalid_id = valid_json.replace(id, "short-id");
        assert!(validate_manifest_content(&invalid_id).is_err());
    }

    #[test]
    fn test_missing_file_detection() {
        let non_existent = Path::new("/tmp/non_existent_kitsupin_binary_12345");
        assert!(!is_file_executable(non_existent));
    }

    #[test]
    fn test_executable_permission_check() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test_bin");
        fs::write(&file_path, "echo hi").unwrap();

        // Initially no exec permission
        fs::set_permissions(&file_path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_file_executable(&file_path));

        // Add exec permission
        fs::set_permissions(&file_path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_file_executable(&file_path));
    }

    #[test]
    fn test_save_user_extension_manifest() {
        let dir = tempdir().unwrap();
        let bin_path = dir.path().join("kitsupin-native-host");
        fs::write(&bin_path, "dummy").unwrap();
        fs::set_permissions(&bin_path, fs::Permissions::from_mode(0o755)).unwrap();

        let id = "abcdefghijklmnopabcdefghijklmnop";
        let manifest_content = generate_native_manifest(&bin_path, id).unwrap();
        let parsed = validate_manifest_content(&manifest_content).unwrap();
        assert_eq!(parsed, id);

        // Invalid extension ID error
        assert!(save_user_extension_manifest("invalid_id").is_err());
    }

    #[test]
    fn test_integration_status_shortcut_options() {
        let dir = tempdir().unwrap();
        let status_none = get_integration_status(dir.path(), None, true);
        assert_eq!(status_none.shortcut_registered, None);
        assert!(!status_none
            .problems
            .iter()
            .any(|p| p.id == "shortcut_conflict"));

        let status_false = get_integration_status(dir.path(), Some(false), true);
        assert_eq!(status_false.shortcut_registered, Some(false));
        assert!(status_false
            .problems
            .iter()
            .any(|p| p.id == "shortcut_conflict"));

        let status_true = get_integration_status(dir.path(), Some(true), true);
        assert_eq!(status_true.shortcut_registered, Some(true));
        assert!(!status_true
            .problems
            .iter()
            .any(|p| p.id == "shortcut_conflict"));
    }
}
