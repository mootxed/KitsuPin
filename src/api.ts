import { invoke } from "@tauri-apps/api/core";
import type { Bootstrap,Category,Clip,ClipQuery,Settings } from "./types";
const tauri = "__TAURI_INTERNALS__" in window;
const demoClips:Clip[]=[
  {id:"sample-1",content:"曖昧さを恐れず、まず小さく試してみる。",contentType:"Text",domain:"youtube.com",pageTitle:"Japanese listening practice — quiet morning",createdAt:new Date().toISOString(),lastCopiedAt:new Date(Date.now()-180_000).toISOString(),copyCount:3,pinned:true,categories:[]},
  {id:"sample-2",content:"https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging",contentType:"Links",domain:"developer.chrome.com",pageTitle:"Native messaging | Chrome for Developers",createdAt:new Date().toISOString(),lastCopiedAt:new Date(Date.now()-3_600_000).toISOString(),copyCount:1,pinned:false,categories:[]},
  {id:"sample-3",content:"A clipboard is most useful when it stays out of your way.",contentType:"Text",domain:null,pageTitle:null,createdAt:new Date().toISOString(),lastCopiedAt:new Date(Date.now()-86_400_000).toISOString(),copyCount:1,pinned:false,categories:[]}
];
const mock:Bootstrap={clips:demoClips,categories:[],settings:{paused:false,autostart:true,shortcut:"Super+V",retentionDays:90,excludedApps:[]}};
export const api={
  bootstrap:(popup:boolean)=>tauri?invoke<Bootstrap>("bootstrap",{popup}):Promise.resolve(mock),
  list:(query:ClipQuery)=>tauri?invoke<Clip[]>("list_clips",{query}):Promise.resolve(demoClips.filter(c=>!query.search||JSON.stringify(c).toLowerCase().includes(query.search.toLowerCase()))),
  copy:(clip:Clip,popup:boolean)=>tauri?invoke("copy_clip",{id:clip.id,content:clip.content,popup}):navigator.clipboard.writeText(clip.content),
  remove:(id:string)=>invoke("delete_clip",{id}),pin:(id:string,pinned:boolean)=>invoke("set_pinned",{id,pinned}),clear:()=>invoke<number>("clear_unpinned"),
  createCategory:(name:string,color:string)=>invoke<Category>("create_category",{name,color}),updateCategory:(id:string,name:string,color:string)=>invoke("update_category",{id,name,color}),deleteCategory:(id:string)=>invoke("delete_category",{id}),
  assign:(clipId:string,categoryId:string)=>invoke("assign_category",{clipId,categoryId}),unassign:(clipId:string,categoryId:string)=>invoke("unassign_category",{clipId,categoryId}),saveSettings:(settings:Settings)=>invoke("save_settings",{settings})
};
export const isTauri=tauri;
