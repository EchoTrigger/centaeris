import react from "@vitejs/plugin-react";
import { defineConfig, type Plugin } from "vite";

function runtimeConfigPlugin(): Plugin {
  return {
    name: "workspace-agent-runtime-config",
    configureServer(server) {
      server.middlewares.use("/config.json", (_request, response) => {
        const apiBaseUrl = process.env.API_BASE_URL;
        if (!apiBaseUrl) {
          response.statusCode = 500;
          response.end("API_BASE_URL is required");
          return;
        }
        response.setHeader("Content-Type", "application/json");
        response.setHeader("Cache-Control", "no-store");
        response.end(JSON.stringify({ apiBaseUrl }));
      });
    },
  };
}

export default defineConfig({
  plugins: [react(), runtimeConfigPlugin()],
  server: {
    host: "0.0.0.0",
    port: 3000,
    strictPort: true,
  },
});
