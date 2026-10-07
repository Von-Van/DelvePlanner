import { configDefaults, defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  envPrefix: ["VITE_"],
  build: { target: ["es2022", "chrome105", "safari13"] },
  // Claude Code worktrees under .claude/ hold their own copies of the tests.
  test: { exclude: [...configDefaults.exclude, ".claude/**"] }
});
