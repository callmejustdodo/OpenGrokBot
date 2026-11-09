import "./new-agent.css";
// SC-004 new agent sheet (the "+" menu is in the sidebar). States: default,
// harness-not-ready (the agent can still be created, US-002), name-taken
// (`agent_create` answers `conflict`), folder-taken. Created: opens its conversation.
// The agent folder is new by default or an existing folder the owner picks;
// repositories are folders picked with the folder picker.
import * as RadioGroup from "@radix-ui/react-radio-group";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import FolderList, { ShortPath } from "../../components/folders/FolderList";
import { addFolders, pickFolders } from "../../components/folders/folders";
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
const NAME_TAKEN = "agent name or slug is taken";
const FOLDER_TAKEN = /^the folder is already (.+)'s folder$/s;
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
  const [folder, setFolder] = useState<string | null>(null);
  const [repositories, setRepositories] = useState<string[]>([]);
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
  const pick = (multiple: boolean, chosen: (folders: string[]) => void) => {
    pickFolders(multiple).then(chosen, () =>
      setError(t("newAgent.pickFailed", "Couldn't open the folder picker. Try again.")),
    );
  };
  const create = () => {
    if (!canCreate) return;
    setError(null);
    act(tauriTransport, PENDING, "agent_create", {
      name: name.trim(),
      role: role.trim(),
      harness,
      avatar: { shape, color },
      folder,
      repositories,
      instructions: instructions.trim() === "" ? null : instructions,
    }).then(
      (agent) => {
        uiStore.getState().navigate({ kind: "agent", agentId: agent.id });
        close();
      },
      (e: unknown) => {
        const data = (e as Partial<CallError> | null)?.data;
        // A taken name and a taken folder both answer `conflict`; formationd's
        // messages for them (crates/formation-core/src/agents/mod.rs) tell them apart.
        const owner = data?.code === "conflict" ? FOLDER_TAKEN.exec(data.message)?.[1] : undefined;
        if (owner !== undefined)
          setError(t("newAgent.folderTakenBy", "This folder is already {{name}}'s folder.", { name: owner }));
        else if (data?.code === "conflict" && folder !== null && data.message !== NAME_TAKEN)
          setError(t("newAgent.folderTaken", "This folder is already another agent's folder."));
        else if (data?.code === "conflict") setNameTaken(true);
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
        <div className="field">
          <span>{t("newAgent.folder", "Agent folder")}</span>
          {folder === null ? (
            <small className="muted">{t("newAgent.folderNew", "A new folder named after the agent")}</small>
          ) : (
            <>
              <ShortPath path={folder} />
              <small className="muted">
                {t("newAgent.folderExistingNote", "Files already in it, including AGENTS.md and CLAUDE.md, are kept.")}
              </small>
            </>
          )}
          <div>
            {folder === null ? (
              <button type="button" className="btn" onClick={() =>
                  pick(false, ([chosen]) => {
                    if (chosen) setFolder(chosen);
                  })
                }>
                {t("newAgent.folderExisting", "Use an existing folder")}
              </button>
            ) : (
              <button type="button" className="btn" onClick={() => setFolder(null)}>
                {t("newAgent.folderUseNew", "Use a new folder")}
              </button>
            )}
          </div>
        </div>
        <div className="field">
          <span>
            {t("newAgent.repositories", "Repositories")} <small>{t("newAgent.optional", "Optional")}</small>
          </span>
          <FolderList
            folders={repositories}
            onRemove={(r) => setRepositories((list) => list.filter((x) => x !== r))}
          />
          <div>
            <button
              type="button"
              className="btn"
              onClick={() => pick(true, (added) => setRepositories((list) => addFolders(list, added)))}
            >
              {t("folders.add", "Add folder")}
            </button>
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
