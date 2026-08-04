export type ContentType = "Text" | "Links" | "Email" | "Numbers";
export type PayloadKind = "text" | "image";
export interface Category { id:string; name:string; color:string; createdAt:number; sortOrder:number }
export interface ImageMetadata { mimeType:string; width:number; height:number; sizeBytes:number; thumbnailDataUrl:string }
export interface ClipSummary { id:string; preview:string; contentLength:number; isTruncated:boolean; contentType:ContentType; payloadKind:PayloadKind; image:ImageMetadata|null; domain:string|null; pageTitle:string|null; createdAt:number; lastCopiedAt:number; copyCount:number; pinned:boolean; categories:Category[] }
export interface ClipDetails { id:string; content:string }
export interface Settings { paused:boolean; autostart:boolean; shortcut:string; retentionDays:number; excludedApps:string[]; maxImageSizeMb:number; maxStorageSizeMb:number }
export interface Bootstrap { clips:ClipSummary[]; categories:Category[]; settings:Settings; invalidSettingsWarning?:boolean }
export interface ClipQuery { search?:string; contentType?:ContentType; payloadKind?:PayloadKind; domain?:string; categoryId?:string; limit?:number; offset?:number }
export type Grouping = "none"|"domain"|"category"|"type";
export interface StorageStats { imageCount:number; imageBytes:number; orphanFilesRemoved:number }

export interface IntegrationProblem {
  id: string;
  severity: "error" | "warning" | "info";
  title: string;
  description: string;
  action: string | null;
}

export interface CapabilityStatus {
  status: "available" | "degraded" | "unavailable" | "failed" | "notTested";
  message?: string;
}

export interface PlatformCapabilities {
  sessionType: string;
  globalClipboardMonitoring: boolean;
  imageClipboard: boolean;
  globalShortcuts: boolean;
  tray: boolean;
  monitoringModeDescription: string;
}

export interface RuntimeCapabilities {
  platform: PlatformCapabilities;
  clipboardMonitoring: CapabilityStatus;
  shortcut: CapabilityStatus;
  tray: CapabilityStatus;
}

export interface IntegrationStatus {
  isLinux: boolean;
  desktopEnvironment: string | null;
  sessionType: string | null;
  isSupportedX11: boolean;
  chromeDetected: boolean;
  extensionId: string | null;
  nativeHostBinaryExists: boolean;
  nativeHostExecutable: boolean;
  nativeManifestExists: boolean;
  nativeManifestValid: boolean;
  chromeManifestValid: boolean;
  chromiumManifestValid: boolean;
  nativeSocketAvailable: boolean;
  nativeMessagingConfigured: boolean;
  nativeMessagingConnected: boolean;
  lastNativeMessageAt: number | null;
  lastExtensionHandshakeAt: number | null;
  lastBrowserCopyMetadataAt: number | null;
  handshakeActive: boolean;
  shortcutRegistered: boolean | null;
  autostartEnabled: boolean;
  platformCapabilities?: PlatformCapabilities;
  runtimeCapabilities?: RuntimeCapabilities;
  problems: IntegrationProblem[];
}

