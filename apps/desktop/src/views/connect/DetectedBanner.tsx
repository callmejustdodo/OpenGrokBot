// SC-005 'detected': the read-only banner above a connected detected agent's
// conversation. It says where the agent was started, that Formation can't send
// it messages, and which operations work (FR-006).
import { useTranslation } from "react-i18next";
import type { AgentId } from "../../ipc/generated/AgentId";
import { useAppStore } from "../../store/store";
import { capabilityRows, harnessLabel } from "./capabilities";

export default function DetectedBanner({ agentId }: { agentId: AgentId }) {
  const { t } = useTranslation();
  const detected = useAppStore((s) => s.mirror?.agents[agentId]?.detected ?? null);
  if (!detected) return null;
  const rows = capabilityRows(detected.capabilities, t);
  return (
    <section className="banner info" data-state="detected" aria-label={t("detected.title", "Read-only · detected agent")}>
      <div className="bt">
        <b>{t("detected.title", "Read-only · detected agent")}</b>
        <p>
          {t(
            "detected.body",
            "This {{harness}} session in {{path}} was started outside Formation, so Formation can't send it messages. You can still see its activity.",
            { harness: harnessLabel(detected.harness, t), path: detected.path },
          )}
        </p>
        <ul aria-label={t("detected.capabilities", "What Formation can do")}>
          {rows.map((c) => (
            <li key={c.key} data-supported={c.supported}>
              {c.supported
                ? t("detected.works", "{{capability}}: works", { capability: c.label })
                : t("detected.doesNotWork", "{{capability}}: doesn't work ({{reason}})", {
                    capability: c.label,
                    reason: c.reason,
                  })}
            </li>
          ))}
        </ul>
      </div>
    </section>
  );
}
