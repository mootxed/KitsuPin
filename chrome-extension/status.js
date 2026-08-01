async function update() {
  const status = document.querySelector("#status"); status.className = ""; status.textContent = "Проверяем Native Host…";
  const result = await chrome.runtime.sendMessage({ event: "status" }).catch(() => ({nativeStatus:"not-installed"}));
  const labels = { connected:"Native Host подключён", unavailable:"KitsuPin сейчас недоступен", "not-installed":"KitsuPin не установлен или Native Host не настроен" };
  status.textContent = labels[result?.nativeStatus] || labels["not-installed"];
  status.className = result?.nativeStatus === "connected" ? "ok" : "error";
}
document.querySelector("#retry").addEventListener("click", update); update();
