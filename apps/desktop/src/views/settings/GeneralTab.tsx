// SC-018 "general": the app language (D-033): follow macOS, Korean or English.
import * as ToggleGroup from "@radix-ui/react-toggle-group";
import { useTranslation } from "react-i18next";
import type { Language } from "../../ipc/generated/Language";
import { applyLanguage } from "./language";
import { useSettingsUpdate } from "./useSettingsUpdate";

type Choice = Language | "system";

export default function GeneralTab() {
  const { t } = useTranslation();
  const { settings, disabled, failed, update } = useSettingsUpdate();
  const value: Choice = settings.language ?? "system";

  const choose = (next: string) => {
    // Radix sends "" when the pressed item is pressed again; keep the choice.
    if (next === "" || next === value) return;
    const language = next === "system" ? null : (next as Language);
    void update({ ...settings, language }).then((saved) => {
      if (saved) applyLanguage(saved.language);
    });
  };

  return (
    <>
      <div className="form-row">
        <span id="settings-language">{t("settings.general.language", "Language")}</span>
        <ToggleGroup.Root
          type="single"
          className="seg"
          aria-labelledby="settings-language"
          value={value}
          onValueChange={choose}
          disabled={disabled}
        >
          <ToggleGroup.Item value="system">{t("settings.general.system", "Follow macOS")}</ToggleGroup.Item>
          <ToggleGroup.Item value="ko">{t("settings.general.korean", "한국어")}</ToggleGroup.Item>
          <ToggleGroup.Item value="en">{t("settings.general.english", "English")}</ToggleGroup.Item>
        </ToggleGroup.Root>
      </div>
      {failed ? (
        <p className="err" role="alert">
          {t("settings.saveFailed", "Couldn't save the change. Try again.")}
        </p>
      ) : null}
    </>
  );
}
