import { fileURLToPath, URL } from 'node:url';
import vue from '@vitejs/plugin-vue';
import { defineConfig } from 'vite';

const isWindows = process.env.TAURI_ENV_PLATFORM === 'windows';

export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ['**/src-tauri/**'],
    },
  },
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  build: {
    // Tauri 在 Windows 使用 Edge WebView2，Linux/macOS 使用 WebKitGTK/WKWebView。
    target: isWindows ? 'chrome105' : 'safari13',
    minify: 'esbuild',
    sourcemap: false,
    // 主窗口与「桌面歌词」悬浮窗是两个独立页面：后者由 Rust 运行时创建并加载
    // lyrics.html，因此必须作为第二个构建入口一同产出到 frontendDist。
    rollupOptions: {
      input: {
        main: fileURLToPath(new URL('./index.html', import.meta.url)),
        lyrics: fileURLToPath(new URL('./lyrics.html', import.meta.url)),
      },
    },
  },
});