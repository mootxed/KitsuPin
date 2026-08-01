export type ContentType = "Text" | "Links" | "Email" | "Numbers";
export interface Category { id:string; name:string; color:string; createdAt:number; sortOrder:number }
export interface ClipSummary { id:string; preview:string; contentLength:number; isTruncated:boolean; contentType:ContentType; domain:string|null; pageTitle:string|null; createdAt:number; lastCopiedAt:number; copyCount:number; pinned:boolean; categories:Category[] }
export interface ClipDetails { id:string; content:string }
export interface Settings { paused:boolean; autostart:boolean; shortcut:string; retentionDays:number; excludedApps:string[] }
export interface Bootstrap { clips:ClipSummary[]; categories:Category[]; settings:Settings; invalidSettingsWarning?:boolean }
export interface ClipQuery { search?:string; contentType?:ContentType; domain?:string; categoryId?:string; limit?:number; offset?:number }
export type Grouping = "none"|"domain"|"category"|"type";
