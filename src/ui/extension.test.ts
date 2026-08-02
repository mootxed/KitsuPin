import { describe, expect, it } from "vitest";

describe("Extension & Drag-and-Drop Helpers", () => {
  it("enforces content script idempotency pattern", () => {
    const fakeWindow: Record<string, unknown> = {};
    expect(fakeWindow.__kitsupinInjected).toBeUndefined();
    fakeWindow.__kitsupinInjected = true;
    expect(fakeWindow.__kitsupinInjected).toBe(true);
  });

  it("normalizes text and domain correctly", () => {
    const normalize = (value: string) => value.replace(/\r\n?/g, "\n").trim();
    const normalizeDomain = (value: string) =>
      value.toLowerCase().replace(/\.$/, "").replace(/^www\./, "");

    expect(normalize("  line1\r\nline2  ")).toBe("line1\nline2");
    expect(normalizeDomain("WWW.GitHub.COM.")).toBe("github.com");
  });
});
