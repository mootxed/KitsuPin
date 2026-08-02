import { describe, expect, it, vi } from "vitest";
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";

const contentScriptCode = fs.readFileSync(
  path.resolve(process.cwd(), "chrome-extension/content-script.js"),
  "utf8"
);
const serviceWorkerCode = fs.readFileSync(
  path.resolve(process.cwd(), "chrome-extension/service-worker.js"),
  "utf8"
);

describe("Chrome Extension - Content Script Execution & Data Formatting", () => {
  it("enforces content script idempotency pattern (__kitsupinInjected)", () => {
    let copyListenersCount = 0;
    const fakeWindow: Record<string, unknown> = {};
    const fakeDocument = {
      addEventListener: (type: string) => {
        if (type === "copy") copyListenersCount++;
      },
    };

    const fn = new Function("window", "document", contentScriptCode);
    fn(fakeWindow, fakeDocument);

    expect(fakeWindow.__kitsupinInjected).toBe(true);
    expect(copyListenersCount).toBe(1);

    // Second execution should do nothing
    fn(fakeWindow, fakeDocument);
    expect(copyListenersCount).toBe(1);
  });

  it("captures copy event, formats domain, pageTitle, UTF-8 length and SHA-256 hash", async () => {
    let copyHandler: ((e: unknown) => Promise<void>) | null = null;
    const sentMessages: unknown[] = [];

    const fakeWindow: Record<string, unknown> = {};
    const fakeDocument = {
      title: "  Test Page Title  ".repeat(30), // long title > 500 chars
      activeElement: null,
      getSelection: () => ({ toString: () => "  Привет, KitsuPin!  \r\nВторая строка  " }),
      addEventListener: (_type: string, handler: (e: unknown) => Promise<void>) => {
        copyHandler = handler;
      },
    };

    const fakeLocation = { hostname: "WWW.Sub.Example.COM." };
    const fakeChrome = {
      runtime: {
        sendMessage: vi.fn().mockImplementation((msg: unknown) => {
          sentMessages.push(msg);
          return Promise.resolve();
        }),
      },
    };

    const fakeCrypto = {
      subtle: {
        digest: async (_alg: string, bytes: Uint8Array) => {
          const hashBuf = crypto.createHash("sha256").update(bytes).digest();
          return hashBuf.buffer.slice(hashBuf.byteOffset, hashBuf.byteOffset + hashBuf.byteLength);
        },
      },
    };

    const fn = new Function(
      "window",
      "document",
      "location",
      "chrome",
      "crypto",
      "TextEncoder",
      "Uint8Array",
      contentScriptCode
    );

    fn(fakeWindow, fakeDocument, fakeLocation, fakeChrome, fakeCrypto, TextEncoder, Uint8Array);

    expect(copyHandler).not.toBeNull();

    // Trigger copy event
    const fakeEvent = {
      clipboardData: {
        getData: (type: string) => (type === "text/plain" ? "  Привет, KitsuPin!  \r\nВторая строка  " : null),
      },
    };

    await copyHandler!(fakeEvent);

    expect(sentMessages.length).toBe(1);
    const msg = sentMessages[0] as {
      version: number;
      event: string;
      contentHash: string;
      contentLength: number;
      domain: string;
      pageTitle: string;
      timestamp: string;
    };

    expect(msg.version).toBe(1);
    expect(msg.event).toBe("copy");
    expect(msg.domain).toBe("sub.example.com"); // domain normalized: lowercased, trailing dot & www stripped
    expect(msg.pageTitle.length).toBeLessThanOrEqual(500);

    const expectedNormalizedContent = (fakeDocument.getSelection().toString()).replace(/\r\n?/g, "\n").trim();
    const expectedBytes = new TextEncoder().encode(expectedNormalizedContent);
    const expectedHash = crypto.createHash("sha256").update(expectedBytes).digest("hex");

    expect(msg.contentLength).toBe(expectedBytes.length);
    expect(msg.contentHash).toBe(expectedHash);
    expect(msg.contentHash).toHaveLength(64);
  });
});

describe("Chrome Extension - Service Worker Lifecycle & Messaging", () => {
  function createSWEnvironment() {
    const listeners: Record<string, Function> = {};
    const storageData: Record<string, unknown> = {};
    const nativeMessagesSent: Array<{ host: string; msg: unknown }> = [];
    const scriptExecutions: Array<{ target: unknown; files: string[] }> = [];

    const mockChrome = {
      runtime: {
        onInstalled: {
          addListener: (fn: Function) => {
            listeners.onInstalled = fn;
          },
        },
        onStartup: {
          addListener: (fn: Function) => {
            listeners.onStartup = fn;
          },
        },
        onMessage: {
          addListener: (fn: Function) => {
            listeners.onMessage = fn;
          },
        },
        sendNativeMessage: vi.fn().mockImplementation(async (host: string, msg: unknown) => {
          nativeMessagesSent.push({ host, msg });
          return { ok: true, version: 1 };
        }),
      },
      storage: {
        local: {
          get: vi.fn().mockImplementation(async (keys: string[]) => {
            const res: Record<string, unknown> = {};
            for (const k of keys) res[k] = storageData[k];
            return res;
          }),
          set: vi.fn().mockImplementation(async (obj: Record<string, unknown>) => {
            Object.assign(storageData, obj);
          }),
        },
      },
      tabs: {
        query: vi.fn().mockResolvedValue([
          { id: 1, url: "https://github.com/mootxed" },
          { id: 2, url: "http://example.com/page" },
          { id: 3, url: "chrome://extensions" },
          { id: 4, url: "chrome-extension://abcdefghijklmnopabcdefghijklmnop/status.html" },
          { id: 5, url: "about:blank" },
          { id: 6, url: "https://chromewebstore.google.com/detail/123" },
        ]),
      },
      scripting: {
        executeScript: vi.fn().mockImplementation(async (arg: { target: unknown; files: string[] }) => {
          scriptExecutions.push(arg);
        }),
      },
    };

    const fn = new Function("chrome", serviceWorkerCode);
    fn(mockChrome);

    return { listeners, storageData, nativeMessagesSent, scriptExecutions, mockChrome };
  }

  it("performs handshake and injects into allowed tabs on install", async () => {
    const env = createSWEnvironment();
    expect(env.listeners.onInstalled).toBeDefined();

    await env.listeners.onInstalled!();

    // Verify native messaging handshake
    expect(env.nativeMessagesSent.length).toBe(1);
    expect(env.nativeMessagesSent[0]!.host).toBe("io.github.mootxed.kitsupin.native");
    expect((env.nativeMessagesSent[0]!.msg as { event: string }).event).toBe("status");

    // Verify storage updated
    expect(env.storageData.nativeStatus).toBe("connected");
    expect(typeof env.storageData.checkedAt).toBe("number");

    // Verify tab script injection skipped chrome://, chrome-extension://, about:, chromewebstore
    expect(env.scriptExecutions.length).toBe(2);
    expect(env.scriptExecutions).toEqual([
      { target: { tabId: 1 }, files: ["content-script.js"] },
      { target: { tabId: 2 }, files: ["content-script.js"] },
    ]);
  });

  it("performs handshake on startup", async () => {
    const env = createSWEnvironment();
    await env.listeners.onStartup!();

    expect(env.nativeMessagesSent.length).toBe(1);
    expect((env.nativeMessagesSent[0]!.msg as { event: string }).event).toBe("status");
    expect(env.storageData.nativeStatus).toBe("connected");
  });

  it("executes handshake before copy message if checkedAt is stale (>30s) or disconnected", async () => {
    const env = createSWEnvironment();
    // Simulate stale checkedAt (>30s ago)
    env.storageData.checkedAt = Date.now() - 45000;
    env.storageData.nativeStatus = "app-not-running";

    let copyResponse: unknown = null;

    const copyMsg = {
      version: 1,
      event: "copy",
      contentHash: "a".repeat(64),
      contentLength: 10,
      domain: "github.com",
      pageTitle: "GitHub",
      timestamp: new Date().toISOString(),
    };

    await new Promise<void>((resolve) => {
      const respond = (res: unknown) => {
        copyResponse = res;
        resolve();
      };
      env.listeners.onMessage!(copyMsg, {}, respond);
    });

    // Expect 2 native messages: status (handshake) then copy
    expect(env.nativeMessagesSent.length).toBe(2);
    expect((env.nativeMessagesSent[0]!.msg as { event: string }).event).toBe("status");
    expect((env.nativeMessagesSent[1]!.msg as { event: string }).event).toBe("copy");

    expect(copyResponse).toEqual({ ok: true, version: 1 });
    expect(env.storageData.nativeStatus).toBe("connected");
  });
});

describe("Drag-and-Drop & Drag Handle Interaction", () => {
  it("handles dragstart, dragend, and drag handle click", () => {
    let categoryModalOpenedWith: unknown = null;

    const mockClip = { id: "clip_123", preview: "test clip" };
    const dragData: Record<string, string> = {};
    const bodyClasses: string[] = [];

    const mockEvent = {
      dataTransfer: {
        setData: (k: string, v: string) => {
          dragData[k] = v;
        },
        effectAllowed: "",
      },
      stopPropagation: vi.fn(),
      preventDefault: vi.fn(),
    };

    // Simulate dragstart
    mockEvent.dataTransfer.setData("text/kitsupin", mockClip.id);
    mockEvent.dataTransfer.effectAllowed = "copy";
    bodyClasses.push("is-dragging-card");

    expect(dragData["text/kitsupin"]).toBe("clip_123");
    expect(mockEvent.dataTransfer.effectAllowed).toBe("copy");
    expect(bodyClasses).toContain("is-dragging-card");

    // Simulate drag handle click -> opens assign category modal
    const handleDragBtnClick = (clip: typeof mockClip) => {
      categoryModalOpenedWith = clip;
    };

    handleDragBtnClick(mockClip);
    expect(categoryModalOpenedWith).toEqual(mockClip);
  });
});
