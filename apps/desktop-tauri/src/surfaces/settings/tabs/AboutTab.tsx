import { useEffect, useState } from "react";
import { useLocale } from "../../../hooks/useLocale";
import { getAppInfo } from "../../../lib/tauri";
import type { AppInfoBridge } from "../../../types/bridge";
import type { TabProps } from "../../Settings";
import codexbarIcon from "../../../assets/codexbar-icon.png";

export default function AboutTab(_props: TabProps) {
  const { t } = useLocale();
  const [appInfo, setAppInfo] = useState<AppInfoBridge | null>(null);

  useEffect(() => {
    void getAppInfo().then(setAppInfo);
  }, []);

  if (!appInfo) {
    return (
      <section className="settings-section">
        <p className="settings-section__hint">{t("AboutLoading")}</p>
      </section>
    );
  }

  return (
    <section className="settings-section about-section">
      <div className="about-header">
        <img className="about-icon" src={codexbarIcon} alt={t("AppName")} />
        <div className="about-title-block">
          <h2 className="about-title">{appInfo.name}</h2>
          <p className="about-version">
            {t("Version")} {appInfo.version}
            {appInfo.buildNumber !== "dev" && ` (${appInfo.buildNumber})`}
          </p>
          <p className="about-tagline">{appInfo.tagline}</p>
        </div>
      </div>

      <div className="about-divider" />
      <p className="about-update-msg">{t("AboutUpdateSourceUnavailable")}</p>

      <p className="about-copyright">MIT License</p>
    </section>
  );
}
