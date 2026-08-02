if (!window.__kitsupinInjected) {
  window.__kitsupinInjected = true;

  const normalize = (value) => value.replace(/\r\n?/g, "\n").trim();
  const normalizeDomain = (value) => value.toLowerCase().replace(/\.$/, "").replace(/^www\./, "");

  async function digestBytes(bytes) {
    const hash = await crypto.subtle.digest("SHA-256", bytes);
    return [...new Uint8Array(hash)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  }

  async function digest(value) {
    return digestBytes(new TextEncoder().encode(value));
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

  async function readCopiedImage() {
    if (!window.navigator?.clipboard?.read) return null;
    try {
      const items = await window.navigator.clipboard.read();
      for (const item of items) {
        const type = ["image/png", "image/jpeg", "image/webp"].find((candidate) => item.types.includes(candidate));
        if (!type) continue;
        const blob = await item.getType(type);
        if (!blob.size || blob.size > 50 * 1024 * 1024) return null;
        const bitmap = await window.createImageBitmap(blob);
        const width = bitmap.width;
        const height = bitmap.height;
        const canvas = typeof window.OffscreenCanvas !== "undefined"
          ? new window.OffscreenCanvas(width, height)
          : Object.assign(document.createElement("canvas"), { width, height });
        const context = canvas.getContext("2d", { willReadFrequently: true });
        if (!context) return null;
        context.drawImage(bitmap, 0, 0);
        const rgba = context.getImageData(0, 0, width, height).data;
        bitmap.close?.();
        const hashInput = new Uint8Array(8 + rgba.byteLength);
        const dimensions = new DataView(hashInput.buffer, 0, 8);
        dimensions.setUint32(0, width, true);
        dimensions.setUint32(4, height, true);
        hashInput.set(new Uint8Array(rgba.buffer, rgba.byteOffset, rgba.byteLength), 8);
        return {
          contentHash: await digestBytes(hashInput),
          contentLength: rgba.byteLength
        };
      }
    } catch {
      // Clipboard Read can be denied by page policy; text capture remains available.
    }
    return null;
  }

  async function sendCopyMetadata(contentHash, contentLength) {
    const domain = normalizeDomain(location.hostname);
    if (!domain) return;
    chrome.runtime.sendMessage({
      version: 1,
      event: "copy",
      contentHash,
      contentLength,
      domain,
      pageTitle: document.title.slice(0, 500),
      timestamp: new Date().toISOString()
    }).catch(() => {});
  }

  document.addEventListener("copy", async (event) => {
    const content = normalize(selectedText(event));
    if (content) {
      const bytes = new TextEncoder().encode(content);
      if (bytes.length > 1_000_000) return;
      const contentHash = await digest(content);
      console.log("[KitsuPin] Captured text copy event:", { contentLength: bytes.length, hashPrefix: contentHash.slice(0, 8) });
      await sendCopyMetadata(contentHash, bytes.length);
      return;
    }
    const image = await readCopiedImage();
    if (image) {
      console.log("[KitsuPin] Captured image copy event:", { contentLength: image.contentLength, hashPrefix: image.contentHash.slice(0, 8) });
      await sendCopyMetadata(image.contentHash, image.contentLength);
    }
  }, true);
}
