import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// Tauri 约定：开发端口固定 1420（tauri.conf.json 的 devUrl 指向它），
// clearScreen 关掉以免清掉 tauri CLI 的输出。
export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    // Tauri v2 在 macOS 用 WKWebView（支持 es2021），Windows 用 WebView2（Chromium）
    target: "es2021",
  },
});
