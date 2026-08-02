import { invoke } from "@tauri-apps/api/core";
import type { Bootstrap, Category, ClipSummary, ClipQuery, Settings, IntegrationStatus } from "./types";

const tauri = "__TAURI_INTERNALS__" in window;

const now = Date.now();
let demoClips: ClipSummary[] = [
  { id: "sample-1", preview: "曖昧さを恐れず、まず小さく試してみる。", contentLength: 21, isTruncated: false, contentType: "Text", domain: "youtube.com", pageTitle: "Japanese listening practice — quiet morning", createdAt: now, lastCopiedAt: now - 180_000, copyCount: 3, pinned: true, categories: [] },
  { id: "sample-2", preview: "https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging", contentLength: 76, isTruncated: false, contentType: "Links", domain: "developer.chrome.com", pageTitle: "Native messaging | Chrome for Developers", createdAt: now, lastCopiedAt: now - 3_600_000, copyCount: 1, pinned: false, categories: [] },
  { id: "sample-3", preview: "A clipboard is most useful when it stays out of your way.", contentLength: 55, isTruncated: false, contentType: "Text", domain: null, pageTitle: null, createdAt: now, lastCopiedAt: now - 86_400_000, copyCount: 1, pinned: false, categories: [] }
];

const mock: Bootstrap = { clips: demoClips, categories: [], settings: { paused: false, autostart: true, shortcut: "Super+V", retentionDays: 90, excludedApps: [] } };

const mockStatus: IntegrationStatus = {
  isLinux: true,
  desktopEnvironment: "KDE",
  sessionType: "x11",
  isSupportedX11: true,
  chromeDetected: true,
  extensionId: null,
  nativeHostBinaryExists: true,
  nativeHostExecutable: true,
  nativeManifestExists: false,
  nativeManifestValid: false,
  chromeManifestValid: false,
  chromiumManifestValid: false,
  nativeSocketAvailable: true,
  nativeMessagingConfigured: false,
  nativeMessagingConnected: false,
  lastNativeMessageAt: null,
  lastExtensionHandshakeAt: null,
  lastBrowserCopyMetadataAt: null,
  handshakeActive: false,
  shortcutRegistered: true,
  autostartEnabled: true,

  problems: [
    {
      id: "manifest_missing",
      severity: "warning",
      title: "Native Messaging manifest отсутствует",
      description: "Chrome-расширение не сможет подключиться к KitsuPin. Установите ID или используйте production .deb пакет.",
      action: "configure_id"
    }
  ]
};

export const api = {
  bootstrap: (popup: boolean) => tauri ? invoke<Bootstrap>("bootstrap", { popup }) : Promise.resolve(mock),
  consumeInvalidSettingsWarning: () => tauri ? invoke<boolean>("consume_invalid_settings_warning") : Promise.resolve(false),
  list: (query: ClipQuery) => tauri ? invoke<ClipSummary[]>("list_clips", { query }) : Promise.resolve(
    demoClips.filter(c => {
      if (query.search && !JSON.stringify(c).toLowerCase().includes(query.search.toLowerCase())) return false;
      if (query.contentType && c.contentType !== query.contentType) return false;
      if (query.domain && c.domain !== query.domain) return false;
      if (query.categoryId && !c.categories.some(cat => cat.id === query.categoryId)) return false;
      return true;
    })
  ),
  getClipContent: (id: string) => tauri ? invoke<string>("get_clip_content", { id }) : Promise.resolve(demoClips.find(c => c.id === id)?.preview || ""),
  copy: (id: string, popup: boolean, content?: string) => tauri ? invoke("copy_clip", { id, popup }) : (content ? navigator.clipboard.writeText(content || "") : Promise.resolve()),
  remove: (id: string) => tauri ? invoke("delete_clip", { id }) : (demoClips = demoClips.filter(c => c.id !== id), mock.clips = demoClips, Promise.resolve()),
  pin: (id: string, pinned: boolean) => tauri ? invoke("set_pinned", { id, pinned }) : (demoClips.forEach(c => { if (c.id === id) c.pinned = pinned; }), Promise.resolve()),
  clear: () => tauri ? invoke<number>("clear_unpinned") : (() => {
    const initialLen = demoClips.length;
    demoClips = demoClips.filter(c => c.pinned);
    mock.clips = demoClips;
    return Promise.resolve(initialLen - demoClips.length);
  })(),
  createCategory: (name: string, color: string) => tauri ? invoke<Category>("create_category", { name, color }) : (() => {
    const cat: Category = { id: `cat-${Date.now()}`, name, color, createdAt: Date.now(), sortOrder: mock.categories.length };
    mock.categories.push(cat);
    return Promise.resolve(cat);
  })(),
  updateCategory: (id: string, name: string, color: string) => tauri ? invoke("update_category", { id, name, color }) : (() => {
    const cat = mock.categories.find(c => c.id === id);
    if (cat) { cat.name = name; cat.color = color; }
    demoClips.forEach(c => c.categories.forEach(cat => { if (cat.id === id) { cat.name = name; cat.color = color; } }));
    return Promise.resolve();
  })(),
  deleteCategory: (id: string) => tauri ? invoke("delete_category", { id }) : (() => {
    mock.categories = mock.categories.filter(c => c.id !== id);
    demoClips.forEach(c => { c.categories = c.categories.filter(cat => cat.id !== id); });
    return Promise.resolve();
  })(),
  assign: (clipId: string, categoryId: string) => tauri ? invoke("assign_category", { clipId, categoryId }) : (() => {
    const clip = demoClips.find(c => c.id === clipId);
    const cat = mock.categories.find(c => c.id === categoryId);
    if (clip && cat && !clip.categories.some(c => c.id === categoryId)) {
      clip.categories.push(cat);
    }
    return Promise.resolve();
  })(),
  unassign: (clipId: string, categoryId: string) => tauri ? invoke("unassign_category", { clipId, categoryId }) : (() => {
    const clip = demoClips.find(c => c.id === clipId);
    if (clip) { clip.categories = clip.categories.filter(c => c.id !== categoryId); }
    return Promise.resolve();
  })(),
  saveSettings: (settings: Settings) => tauri ? invoke("save_settings", { settings }) : (mock.settings = settings, Promise.resolve()),
  getIntegrationStatus: () => tauri ? invoke<IntegrationStatus>("get_integration_status") : Promise.resolve(mockStatus),
  configureExtensionId: (extensionId: string) => tauri ? invoke<IntegrationStatus>("configure_extension_id", { extensionId }) : Promise.resolve({ ...mockStatus, extensionId, nativeManifestExists: true, nativeManifestValid: true, problems: [] }),
  openExtensionDir: () => tauri ? invoke<string>("open_extension_dir") : Promise.resolve("chrome-extension"),
  openChromeExtensionsPage: () => tauri ? invoke("open_chrome_extensions_page") : Promise.resolve()
};

export const isTauri = tauri;
