// SC-018 Settings window: General (language), Harnesses, Notifications and
// Team tabs. Every change goes to formationd; the confirming event updates
// the mirror, which every window shares through its own store.
import * as Tabs from "@radix-ui/react-tabs";
import { useTranslation } from "react-i18next";
import { useAppStore } from "../../store/store";
import GeneralTab from "./GeneralTab";
import HarnessesTab from "./HarnessesTab";
import NotificationsTab from "./NotificationsTab";
import TeamTab from "./TeamTab";
import "./settings.css";

export type SettingsTab = "general" | "harnesses" | "notifications" | "team";

export default function Settings({ initialTab = "general" }: { initialTab?: SettingsTab }) {
  const { t } = useTranslation();
  const ready = useAppStore((s) => s.mirror !== null);
  const connected = useAppStore((s) => s.connected);
  return (
    <main className="settings" aria-label={t("settings.label", "Settings")} data-connected={connected}>
      {connected ? null : (
        <p className="banner warn">
          {ready
            ? t("settings.disconnected", "Lost the connection to formationd. Changes are off until it is back.")
            : t("settings.connecting", "Connecting to formationd…")}
        </p>
      )}
      <Tabs.Root defaultValue={initialTab}>
        <Tabs.List className="stabs" aria-label={t("settings.label", "Settings")}>
          <Tabs.Trigger value="general">{t("settings.tab.general", "General")}</Tabs.Trigger>
          <Tabs.Trigger value="harnesses">{t("settings.tab.harnesses", "Harnesses")}</Tabs.Trigger>
          <Tabs.Trigger value="notifications">{t("settings.tab.notifications", "Notifications")}</Tabs.Trigger>
          <Tabs.Trigger value="team">{t("settings.tab.team", "Team")}</Tabs.Trigger>
        </Tabs.List>
        {ready ? (
          <>
            <Tabs.Content value="general" className="wbody">
              <GeneralTab />
            </Tabs.Content>
            <Tabs.Content value="harnesses" className="wbody">
              <HarnessesTab />
            </Tabs.Content>
            <Tabs.Content value="notifications" className="wbody">
              <NotificationsTab />
            </Tabs.Content>
            <Tabs.Content value="team" className="wbody">
              <TeamTab />
            </Tabs.Content>
          </>
        ) : null}
      </Tabs.Root>
    </main>
  );
}
