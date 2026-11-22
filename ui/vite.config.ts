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
    // 主窗口、桌面歌词悬浮窗与歌词设置悬浮窗是三个独立页面：后两者由 Rust
    // 运行时创建并分别加载 lyrics.html / lyrics-settings.html，必须一同产出。
    rollupOptions: {
      input: {
        main: fileURLToPath(new URL('./index.html', import.meta.url)),
        lyrics: fileURLToPath(new URL('./lyrics.html', import.meta.url)),
        lyricsSettings: fileURLToPath(new URL('./lyrics-settings.html', import.meta.url)),
      },
    },
  },
});