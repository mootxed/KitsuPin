const normalize = (value) => value.replace(/\r\n?/g, "\n").trim();
const normalizeDomain = (value) => value.toLowerCase().replace(/\.$/, "").replace(/^www\./, "");
async function digest(value) {
  const bytes = new TextEncoder().encode(value);
  const hash = await crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(hash)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}
function selectedText(event) {
  const clipboardText = event.clipboardData?.getData("text/plain");
  if (clipboardText) return clipboardText;
  const active = document.activeElement;
  if ((active instanceof HTMLInputElement || active instanceof HTMLTextAreaElement) && active.selectionStart !== null && active.selectionEnd !== null) {
    return active.value.slice(active.selectionStart, active.selectionEnd);
  }
  return document.getSelection()?.toString() || "";
}
document.addEventListener("copy", async (event) => {
  const content = normalize(selectedText(event));
  if (!content) return;
  const bytes = new TextEncoder().encode(content);
  if (bytes.length > 1_000_000) return;
  const domain = normalizeDomain(location.hostname);
  if (!domain) return;
  chrome.runtime.sendMessage({
    version: 1, event: "copy", contentHash: await digest(content),
    contentLength: bytes.length,
    domain, pageTitle: document.title.slice(0, 500), timestamp: new Date().toISOString()
  }).catch(() => {});
}, true);
