import js from "@eslint/js";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist", "src-tauri/target", "chrome-extension"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  { files: ["src/**/*.ts"], languageOptions: { globals: { document: "readonly", window: "readonly", HTMLElement: "readonly", KeyboardEvent: "readonly", HTMLInputElement: "readonly", DragEvent: "readonly" } } },
  { files: ["scripts/**/*.mjs"], languageOptions: { globals: { Buffer: "readonly" } } }
);
