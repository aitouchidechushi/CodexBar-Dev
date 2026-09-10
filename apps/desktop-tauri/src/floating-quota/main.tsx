import React from "react";
import ReactDOM from "react-dom/client";
import { LocaleProvider } from "../i18n/LocaleProvider";
import { useLocale } from "../hooks/useLocale";
import { getBootstrapState } from "../lib/tauri";
import "../styles.css";
import FloatingQuota from "./FloatingQuota";
import "./FloatingQuota.css";

const root = ReactDOM.createRoot(document.getElementById("root")!);

function StartupError() {
  const { t } = useLocale();
  return (
    <div className="floating-quota floating-quota__empty">
      {t("ProviderStatusError")}
    </div>
  );
}

getBootstrapState()
  .then((state) => {
    root.render(
      <React.StrictMode>
        <LocaleProvider>
          <FloatingQuota state={state} />
        </LocaleProvider>
      </React.StrictMode>,
    );
  })
  .catch(() => {
    root.render(
      <LocaleProvider>
        <StartupError />
      </LocaleProvider>,
    );
  });
