import js from "@eslint/js";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist", "**/target"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  { files: ["src/**/*.ts"], languageOptions: { globals: { document: "readonly", window: "readonly", HTMLElement: "readonly", KeyboardEvent: "readonly", HTMLInputElement: "readonly", DragEvent: "readonly" } } },
  { files: ["chrome-extension/**/*.js"], languageOptions: { globals: { chrome: "readonly", document: "readonly", location: "readonly", crypto: "readonly", window: "readonly", console: "readonly", fetch: "readonly", setTimeout: "readonly", TextEncoder: "readonly", HTMLInputElement: "readonly", HTMLTextAreaElement: "readonly" } } },
  { files: ["scripts/**/*.mjs"], languageOptions: { globals: { Buffer: "readonly" } } }
);
