import { defineConfig } from "vite";

export default defineConfig({
  publicDir: "generated-public",
  worker: {
    format: "es",
  },
  build: {
    target: "es2022",
  },
});
