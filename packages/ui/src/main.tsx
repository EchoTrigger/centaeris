import { LucideProvider } from "lucide-react";
import { MotionConfig } from "motion/react";
import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { AppErrorBoundary } from "./AppErrorBoundary";
import "./index.css";

document.documentElement.dataset.platform = /Mac/i.test(navigator.platform)
  ? "darwin"
  : /Win/i.test(navigator.platform)
    ? "win32"
    : "linux";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <AppErrorBoundary>
      <LucideProvider strokeWidth={1.5}>
        <MotionConfig reducedMotion="user"><App /></MotionConfig>
      </LucideProvider>
    </AppErrorBoundary>
  </React.StrictMode>
);
