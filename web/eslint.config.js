// SPDX-License-Identifier: GPL-3.0-or-later
// Lint: the recommended JS and TypeScript rules (syntax-only, so it stays fast; `npm run
// typecheck` does the type checking), plus the web edition's security rules
// (docs/WEB_PROMPT.md §5): provider text is never rendered as HTML, and nothing evaluates code.
import js from "@eslint/js";
import tseslint from "typescript-eslint";

const htmlSinks = ["innerHTML", "outerHTML", "insertAdjacentHTML", "srcdoc"].map((property) => ({
  property,
  message: "Never build markup from data: render text with JSX (WEB_PROMPT.md §5).",
}));

export default tseslint.config(
  { ignores: ["dist/", "node_modules/", ".media/"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["**/*.{ts,tsx,js}"],
    languageOptions: {
      ecmaVersion: 2023,
      sourceType: "module",
      globals: {
        window: "readonly",
        document: "readonly",
        self: "readonly",
        caches: "readonly",
        fetch: "readonly",
        Response: "readonly",
        URL: "readonly",
        console: "readonly",
        process: "readonly",
      },
    },
    rules: {
      "no-restricted-properties": ["error", ...htmlSinks],
      "no-restricted-syntax": [
        "error",
        { selector: "JSXAttribute[name.name='dangerouslySetInnerHTML']", message: "No dangerouslySetInnerHTML (WEB_PROMPT.md §5)." },
        { selector: "CallExpression[callee.property.name='write'][callee.object.name='document']", message: "No document.write." },
        { selector: "CallExpression[callee.name='eval']", message: "No eval." },
        { selector: "NewExpression[callee.name='Function']", message: "No new Function." },
        { selector: "CallExpression[callee.property.name='setAttribute'][arguments.0.value='style']", message: "Inline style attributes break the CSP: use classes or element.style." },
      ],
      "no-console": ["error", { allow: ["warn", "error"] }],
      eqeqeq: ["error", "always"],
      "@typescript-eslint/no-unused-vars": ["error", { argsIgnorePattern: "^_", varsIgnorePattern: "^_" }],
      "@typescript-eslint/consistent-type-imports": "error",
    },
  },
  {
    files: ["mock/**", "vite.config.ts", "scripts/**"],
    rules: { "no-console": "off" },
  },
);
