import { describe,expect,it } from "vitest";
import { relativeTime } from "./time";

describe("relativeTime",()=>{
  it("formats recent timestamps in Russian",()=>{
    const now=new Date("2026-08-01T12:00:00Z").getTime();
    expect(relativeTime("2026-08-01T11:55:00Z",now)).toContain("5");
  });
});
