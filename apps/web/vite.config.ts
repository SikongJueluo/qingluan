import { fileURLToPath, URL } from "node:url";

import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
import vueDevTools from "vite-plugin-vue-devtools";
import tailwindcss from "@tailwindcss/vite";

// https://vite.dev/config/
export default defineConfig({
  plugins: [vue(), vueDevTools(), tailwindcss()],
  // Dev only: proxy daemon API so the same-origin client works under
  // `vite dev` (in production the daemon serves this bundle itself).
  server: {
    proxy: {
      "/reviews": "http://127.0.0.1:47129",
      "/health": "http://127.0.0.1:47129",
    },
  },
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
});
