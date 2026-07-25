import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react()],

  // 防止 Vite 清屏时把 Tauri 的 Rust 编译输出一并清掉
  clearScreen: false,

  // Tauri dev 要求固定端口
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // 不监听 src-tauri 的 Rust 构建产物
      ignored: ["**/src-tauri/**"],
    },
  },

  build: {
    outDir: "dist",
    // 多页面入口：index.html = 用量面板，settings.html = 设置窗口
    rollupOptions: {
      input: {
        main: fileURLToPath(new URL("./index.html", import.meta.url)),
        settings: fileURLToPath(new URL("./settings.html", import.meta.url)),
      },
    },
  },
});
