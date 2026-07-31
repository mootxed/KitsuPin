export type ContentType = "Text" | "Links" | "Email" | "Numbers";
export interface Category { id:string; name:string; color:string; createdAt:string; sortOrder:number }
export interface Clip { id:string; content:string; contentType:ContentType; domain:string|null; pageTitle:string|null; createdAt:string; lastCopiedAt:string; copyCount:number; pinned:boolean; categories:Category[] }
export interface Settings { paused:boolean; autostart:boolean; shortcut:string; retentionDays:number; excludedApps:string[] }
export interface Bootstrap { clips:Clip[]; categories:Category[]; settings:Settings }
export interface ClipQuery { search?:string; contentType?:ContentType; domain?:string; categoryId?:string; limit?:number; offset?:number }
export type Grouping = "none"|"domain"|"category"|"type";
