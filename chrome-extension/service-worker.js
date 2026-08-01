const HOST = "io.github.mootxed.kitsupin.native";
async function send(message) {
  try {
    const response = await chrome.runtime.sendNativeMessage(HOST, message);
    const connected = response?.ok === true;
    await chrome.storage.local.set({ nativeStatus: connected ? "connected" : "unavailable", checkedAt: Date.now() });
    return connected;
  } catch {
    await chrome.storage.local.set({ nativeStatus: "not-installed", checkedAt: Date.now() });
    return false;
  }
}
chrome.runtime.onMessage.addListener((message, _sender, respond) => {
  if (message?.event === "status") {
    send({version:1,event:"status"}).then(() => chrome.storage.local.get(["nativeStatus","checkedAt"])).then(respond);
  } else {
    send(message).then(respond);
  }
  return true;
});
