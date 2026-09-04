import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";

document.documentElement.dataset.platform = /Mac/i.test(navigator.platform)
  ? "darwin"
  : /Win/i.test(navigator.platform)
    ? "win32"
    : "linux";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
