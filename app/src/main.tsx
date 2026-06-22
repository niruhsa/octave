import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { installViewportSync } from "./lib/viewport";
import "./index.css";

// Track the visual viewport so the mobile soft keyboard can't hide focused
// inputs (see lib/viewport.ts). Must run before first paint sets the height.
installViewportSync();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
