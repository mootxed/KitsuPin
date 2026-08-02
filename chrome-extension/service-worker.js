const HOST = "io.github.mootxed.kitsupin.native";

async function performHandshake() {
  try {
    const response = await chrome.runtime.sendNativeMessage(HOST, {
      version: 1,
      event: "status",
      timestamp: new Date().toISOString(),
      extensionVersion: "0.1.2"
    });
    if (response?.ok === true) {
      await chrome.storage.local.set({ nativeStatus: "connected", checkedAt: Date.now(), errorDetail: null });
      return true;
    } else {
      const err = response?.error === "app_not_running" ? "app-not-running" : "unknown-error";
      await chrome.storage.local.set({ nativeStatus: err, checkedAt: Date.now(), errorDetail: response?.error || null });
      return false;
    }
  } catch (err) {
    const message = String(err?.message || err);
    let status = "not-installed";
    if (message.includes("specified native messaging host not found")) {
      status = "not-installed";
    } else if (message.includes("Access to the specified native messaging host is forbidden")) {
      status = "manifest-invalid";
    } else {
      status = "not-installed";
    }
    await chrome.storage.local.set({ nativeStatus: status, checkedAt: Date.now(), errorDetail: message });
    return false;
  }
}

async function injectIntoOpenTabs() {
  try {
    const tabs = await chrome.tabs.query({ url: ["http://*/*", "https://*/*"] });
    for (const tab of tabs) {
      if (!tab.id || !tab.url) continue;
      if (
        tab.url.startsWith("chrome://") ||
        tab.url.startsWith("chrome-extension://") ||
        tab.url.startsWith("about:") ||
        tab.url.startsWith("edge://") ||
        tab.url.includes("chromewebstore.google.com")
      ) {
        continue;
      }
      chrome.scripting.executeScript({
        target: { tabId: tab.id },
        files: ["content-script.js"]
      }).catch(() => {});
    }
  } catch (err) {
    console.warn("[KitsuPin] Tab injection warning:", err);
  }

}

chrome.runtime.onInstalled.addListener(async () => {
  await performHandshake();
  await injectIntoOpenTabs();
});

chrome.runtime.onStartup.addListener(async () => {
  await performHandshake();
});

chrome.runtime.onMessage.addListener((message, _sender, respond) => {
  if (message?.event === "status") {
    performHandshake()
      .then(() => chrome.storage.local.get(["nativeStatus", "checkedAt", "errorDetail"]))
      .then(respond);
    return true;
  }

  if (message?.event === "copy") {
    (async () => {
      try {
        const data = await chrome.storage.local.get(["checkedAt", "nativeStatus"]);
        const age = Date.now() - (data.checkedAt || 0);
        if (age > 30000 || data.nativeStatus !== "connected") {
          await performHandshake();
        }
        const res = await chrome.runtime.sendNativeMessage(HOST, message);
        if (res?.ok === true) {
          await chrome.storage.local.set({
            nativeStatus: "connected",
            checkedAt: Date.now(),
            errorDetail: null
          });
        } else {
          const nativeStatus =
            res?.error === "app_not_running"
              ? "app-not-running"
              : "unknown-error";

          await chrome.storage.local.set({
            nativeStatus,
            checkedAt: Date.now(),
            errorDetail: res?.error ?? "unknown-error"
          });
        }
        respond(res);
      } catch (err) {
        await performHandshake();
        respond({ ok: false, error: String(err) });
      }
    })();
    return true;
  }

  return false;
});

