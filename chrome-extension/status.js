async function update() {
  const status = document.querySelector("#status");
  status.className = "";
  status.textContent = "Проверяем Native Host…";
  const result = await chrome.runtime.sendMessage({ event: "status" }).catch(() => ({ nativeStatus: "not-installed" }));
  const labels = {
    connected: "Native Host подключён",
    "app-not-running": "KitsuPin сейчас недоступен. Запустите приложение и повторите проверку.",
    "not-installed": "KitsuPin не установлен или Native Host не настроен.",
    "manifest-invalid": "Некорректный Native Messaging manifest."
  };
  status.textContent = labels[result?.nativeStatus] || labels["not-installed"];
  status.className = result?.nativeStatus === "connected" ? "ok" : "error";
}
document.querySelector("#retry").addEventListener("click", update);
update();

