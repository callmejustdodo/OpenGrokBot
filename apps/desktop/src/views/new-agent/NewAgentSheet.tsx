// SC-004 new agent sheet (the "+" menu is in the sidebar). States: default,
// harness-not-ready (the agent can still be created, US-002), name-taken
// (`agent_create` answers `conflict`). Created: opens its conversation.
import * as RadioGroup from "@radix-ui/react-radio-group";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import AgentAvatar from "../../components/sidebar/AgentAvatar";
import type { CallError } from "../../ipc/client";
import type { HarnessKind } from "../../ipc/generated/HarnessKind";
import type { HarnessStatus } from "../../ipc/generated/HarnessStatus";
import { tauriTransport } from "../../ipc/transport";
import { useAppStore } from "../../store/store";
import { act } from "../../store/sync";
import { uiStore } from "../../store/ui";
import Sheet from "../../ui/Sheet";
import { AVATAR_COLORS, AVATAR_SHAPES, isReady } from "./options";

const PENDING = "agent_create";
const HARNESSES: { kind: Exclude<HarnessKind, "detected">; name: string }[] = [
  { kind: "claude_code", name: "Claude Code" },
  { kind: "codex", name: "Codex" },
];

export default function NewAgentSheet() {
  const { t } = useTranslation();
  const harnesses = useAppStore((s) => s.mirror?.harnesses);
  const creating = useAppStore((s) => PENDING in s.pending);
  const [name, setName] = useState("");
  const [role, setRole] = useState("");
  const [harness, setHarness] = useState<Exclude<HarnessKind, "detected">>("claude_code");
  const [shape, setShape] = useState(AVATAR_SHAPES[0]);
  const [color, setColor] = useState(AVATAR_COLORS[0]);
  const [instructions, setInstructions] = useState("");
  const [nameTaken, setNameTaken] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const status = (kind: HarnessKind): HarnessStatus | undefined => harnesses?.find((h) => h.harness === kind);
  const statusLine = (kind: HarnessKind): string => {
    const s = status(kind);
    if (!s || !s.installed) return t("newAgent.harness.notInstalled", "Not installed");
    if (isReady(s)) return t("newAgent.harness.ready", "Installed · signed in");
    return s.problem ?? t("newAgent.harness.signInNeeded", "Installed · sign-in needed");
  };
  const chosen = status(harness);
  const chosenReady = chosen ? isReady(chosen) : false;
  const canCreate = name.trim() !== "" && role.trim() !== "" && !nameTaken && !creating;

  const close = () => uiStore.getState().closeSheet();
  const create = () => {
    if (!canCreate) return;
    setError(null);
    act(tauriTransport, PENDING, "agent_create", {
      name: name.trim(),
      role: role.trim(),
      harness,
      avatar: { shape, color },
      folder: null,
      repositories: [],
      instructions: instructions.trim() === "" ? null : instructions,
    }).then(
      (agent) => {
        uiStore.getState().navigate({ kind: "agent", agentId: agent.id });
        close();
      },
      (e: unknown) => {
        if ((e as Partial<CallError> | null)?.data?.code === "conflict") setNameTaken(true);
        else setError(t("newAgent.error", "Couldn't create the agent. Try again."));
      },
    );
  };

  return (
    <Sheet
      open
      onOpenChange={(open) => (open ? undefined : close())}
      title={t("newAgent.title", "New agent")}
      lead={t("newAgent.lead", "Each role gets one agent and its own folder.")}
      footer={
        <>
          <span className="muted small spacer">{t("newAgent.startsStopped", "Starts private and stopped")}</span>
          <button type="button" className="btn" onClick={close}>
            {t("newAgent.cancel", "Cancel")}
          </button>
          <button type="button" className="btn primary" disabled={!canCreate} onClick={create}>
            {t("newAgent.create", "Create")}
          </button>
        </>
      }
    >
      <form
        className="new-agent"
        data-state={nameTaken ? "name-taken" : chosenReady ? "default" : "harness-not-ready"}
        onSubmit={(e) => {
          e.preventDefault();
          create();
        }}
      >
        <div className="field">
          <label>
            <span>{t("newAgent.name", "Name")}</span>
            <input
              className={nameTaken ? "input invalid" : "input"}
              required
              value={name}
              aria-invalid={nameTaken}
              aria-describedby={nameTaken ? "new-agent-name-taken" : undefined}
              onChange={(e) => {
                setName(e.target.value);
                setNameTaken(false);
              }}
            />
          </label>
          {nameTaken ? (
            <span id="new-agent-name-taken" className="err" role="alert">
              {t("newAgent.nameTaken", "An agent with this name already exists.")}
            </span>
          ) : null}
        </div>
        <label className="field">
          <span>
            {t("newAgent.role", "Role")} <small>{t("newAgent.roleHint", "Any role, not only coding")}</small>
          </span>
          <input className="input" required value={role} onChange={(e) => setRole(e.target.value)} />
        </label>
        <div className="field">
          <span id="new-agent-harness">{t("newAgent.harness.label", "Harness")}</span>
          <RadioGroup.Root
            aria-labelledby="new-agent-harness"
            value={harness}
            onValueChange={(v) => setHarness(v as Exclude<HarnessKind, "detected">)}
          >
            {HARNESSES.map((h) => {
              const s = status(h.kind);
              return (
                <RadioGroup.Item
                  key={h.kind}
                  value={h.kind}
                  className="harness"
                  data-ready={s ? isReady(s) : false}
                  aria-label={`${h.name} · ${statusLine(h.kind)}`}
                >
                  <span className="hn">
                    <b>{h.name}</b>
                    <small>{statusLine(h.kind)}</small>
                  </span>
                  <RadioGroup.Indicator className="radio" />
                </RadioGroup.Item>
              );
            })}
          </RadioGroup.Root>
          {!chosenReady ? (
            <p className="note" role="note">
              {t(
                "newAgent.harness.notReady",
                "You can create it, but it can't start until the harness is ready.",
              )}
            </p>
          ) : null}
        </div>
        <div className="field">
          <span>{t("newAgent.avatar", "Avatar")}</span>
          <AgentAvatar avatar={{ shape, color }} size={40} />
          <div className="shapes" role="group" aria-label={t("newAgent.shape", "Shape")}>
            {AVATAR_SHAPES.map((s) => (
              <button
                key={s}
                type="button"
                className="shape-btn"
                data-shape={s}
                aria-pressed={s === shape}
                aria-label={s}
                onClick={() => setShape(s)}
              />
            ))}
          </div>
          <div className="swatches" role="group" aria-label={t("newAgent.color", "Color")}>
            {AVATAR_COLORS.map((c) => (
              <button
                key={c}
                type="button"
                className="swatch"
                data-color={c}
                aria-pressed={c === color}
                aria-label={c}
                onClick={() => setColor(c)}
              />
            ))}
          </div>
        </div>
        <label className="field">
          <span>
            {t("newAgent.instructions", "Starting instructions")} <small>{t("newAgent.optional", "Optional")}</small>
          </span>
          <textarea className="textarea" value={instructions} onChange={(e) => setInstructions(e.target.value)} />
        </label>
        {error ? (
          <p className="err" role="alert">
            {error}
          </p>
        ) : null}
      </form>
    </Sheet>
  );
}
