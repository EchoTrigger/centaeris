import { t } from "./i18n";
import { createRoot } from "react-dom/client";
import { RouterProvider } from "react-router/dom";
import { configureApi } from "./api";
import { loadRuntimeConfig } from "./config";
import { createRouter } from "./router";
import "./globals.css";

const rootElement = document.getElementById("root");
if (!rootElement) throw new Error("missing #root element");
const root = createRoot(rootElement);

loadRuntimeConfig()
  .then((config) => {
    configureApi(config);
    root.render(<RouterProvider router={createRouter()} />);
  })
  .catch((error: unknown) => {
    const message = error instanceof Error ? error.message : String(error);
    root.render(<main role="alert"><h1>{t("main.centaerisCouldNotStart")}</h1><p>{message}</p></main>);
    console.error(error);
  });
