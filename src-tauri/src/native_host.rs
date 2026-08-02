use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
};

const MAX_MESSAGE: usize = 16 * 1024;

fn data_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .map(|p| p.join("kitsupin/native.sock"))
}

fn read_message() -> anyhow::Result<Vec<u8>> {
    let mut len = [0_u8; 4];
    std::io::stdin().read_exact(&mut len)?;
    let size = u32::from_ne_bytes(len) as usize;
    anyhow::ensure!(size <= MAX_MESSAGE, "message too large");
    let mut body = vec![0; size];
    std::io::stdin().read_exact(&mut body)?;
    Ok(body)
}

fn respond(value: Value) {
    if let Ok(body) = serde_json::to_vec(&value) {
        let _ = std::io::stdout().write_all(&(body.len() as u32).to_ne_bytes());
        let _ = std::io::stdout().write_all(&body);
        let _ = std::io::stdout().flush();
    }
}

fn handle() -> anyhow::Result<()> {
    let body = read_message()?;
    let value: Value = serde_json::from_slice(&body)?;

    if is_status_probe(&value) {
        // Valid status probe
    } else if is_copy_event(&value) {
        let copy_event: crate::browser_metadata::BrowserCopyEvent =
            serde_json::from_value(value.clone())
                .map_err(|e| anyhow::anyhow!("invalid_message: {e}"))?;
        copy_event
            .validate()
            .map_err(|e| anyhow::anyhow!("invalid_message: {e}"))?;
    } else {
        anyhow::bail!("invalid_message");
    }

    let path = data_dir().ok_or_else(|| anyhow::anyhow!("data directory unavailable"))?;
    let mut socket =
        UnixStream::connect(path).map_err(|e| anyhow::anyhow!("app_not_running: {e}"))?;

    socket.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
    socket.set_write_timeout(Some(std::time::Duration::from_secs(2)))?;

    serde_json::to_writer(&mut socket, &value)?;
    socket.write_all(b"\n")?;
    socket.flush()?;

    use std::io::BufRead;
    let mut response_line = String::new();
    let mut reader = std::io::BufReader::new(socket);
    reader.read_line(&mut response_line)?;

    let resp_val: Value = serde_json::from_str(&response_line)
        .map_err(|_| anyhow::anyhow!("app_response_invalid"))?;

    if resp_val.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(())
    } else {
        let err = resp_val
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("app_rejected");
        anyhow::bail!("app_rejected: {err}");
    }
}

fn is_status_probe(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        object.get("version").and_then(Value::as_u64) == Some(1)
            && object.get("event").and_then(Value::as_str) == Some("status")
    })
}

fn is_copy_event(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        object.get("version").and_then(Value::as_u64) == Some(1)
            && object.get("event").and_then(Value::as_str) == Some("copy")
    })
}

pub fn run() {
    match handle() {
        Ok(()) => respond(json!({"ok": true, "version": 1})),
        Err(error) => {
            let err_str = error.to_string();
            let err_code = if err_str.contains("app_not_running") {
                "app_not_running"
            } else if err_str.contains("invalid_message") {
                "invalid_message"
            } else {
                "host_unavailable"
            };
            eprintln!("KitsuPin native host error: {err_code}");
            respond(json!({"ok": false, "version": 1, "error": err_code}));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_probe_is_narrow_and_versioned() {
        assert!(is_status_probe(&json!({"version": 1, "event": "status"})));
        assert!(is_status_probe(
            &json!({"version": 1, "event": "status", "timestamp": "123"})
        ));
        assert!(!is_status_probe(&json!({"version": 2, "event": "status"})));
    }

    #[test]
    fn copy_event_is_narrow_and_versioned() {
        assert!(is_copy_event(&json!({"version": 1, "event": "copy"})));
        assert!(!is_copy_event(&json!({"version": 1, "event": "other"})));
    }
}
