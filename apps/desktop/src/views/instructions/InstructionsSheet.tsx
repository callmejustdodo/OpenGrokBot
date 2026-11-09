// SC-006 instructions and skills sheet: reads the agent's files on open, edits
// AGENTS.md, and saves with ⌘S against the revision it read. States: default
// (saved), unsaved, changed-on-disk (the agent's `files_revision` moved while I
// was editing, or the save answered `conflict`).
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { CallError } from "../../ipc/client";
import type { AgentFiles } from "../../ipc/generated/AgentFiles";
import type { AgentId } from "../../ipc/generated/AgentId";
import { tauriTransport } from "../../ipc/transport";
import { useAppStore } from "../../store/store";
import { act } from "../../store/sync";
import { uiStore } from "../../store/ui";
import Sheet from "../../ui/Sheet";

const INSTRUCTIONS = "AGENTS.md";

type Loaded = { revision: string; saved: string; skills: string[] };

function fromFiles(files: AgentFiles): Loaded {
  return {
    revision: files.revision,
    saved: files.files.find((f) => f.path === INSTRUCTIONS)?.content ?? "",
    skills: files.files.filter((f) => f.path !== INSTRUCTIONS).map((f) => f.path),
  };
}

export default function InstructionsSheet({ agentId }: { agentId: AgentId }) {
  const { t } = useTranslation();
  const agent = useAppStore((s) => s.mirror?.agents[agentId]);
  const saving = useAppStore((s) => `agent_files_write:${agentId}` in s.pending);
  const [loaded, setLoaded] = useState<Loaded | null>(null);
  const [draft, setDraft] = useState("");
  const [conflict, setConflict] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const read = useCallback(() => {
    act(tauriTransport, `agent_files_read:${agentId}`, "agent_files_read", { agent_id: agentId }).then(
      (files) => {
        const next = fromFiles(files);
        setLoaded(next);
        setDraft(next.saved);
        setConflict(false);
        setError(null);
      },
      () => setError(t("instructions.readError", "Couldn't read the agent's files.")),
    );
  }, [agentId, t]);

  useEffect(() => read(), [read]);

  const dirty = loaded !== null && draft !== loaded.saved;
  const diskRevision = agent?.files_revision;
  const changedOnDisk = loaded !== null && (conflict || (diskRevision !== undefined && diskRevision !== loaded.revision));

  // Edits made elsewhere while I have nothing unsaved: just take them.
  useEffect(() => {
    if (loaded && !dirty && !conflict && diskRevision !== undefined && diskRevision !== loaded.revision) read();
  }, [loaded, dirty, conflict, diskRevision, read]);

  const save = () => {
    if (!loaded || !dirty || saving) return;
    setError(null);
    act(tauriTransport, `agent_files_write:${agentId}`, "agent_files_write", {
      agent_id: agentId,
      path: INSTRUCTIONS,
      content: draft,
      base_revision: loaded.revision,
    }).then(
      (files) => {
        setLoaded({ ...fromFiles(files), saved: draft });
        setConflict(false);
      },
      (e: unknown) => {
        if ((e as Partial<CallError> | null)?.data?.code === "conflict") setConflict(true);
        else setError(t("instructions.saveError", "Couldn't save. Try again."));
      },
    );
  };

  /** Keep my edits: the next save replaces what changed on disk. */
  const keepMine = () => {
    if (loaded && diskRevision !== undefined) setLoaded({ ...loaded, revision: diskRevision });
    setConflict(false);
  };

  const close = () => uiStore.getState().closeSheet();
  const state = changedOnDisk && dirty ? "changed-on-disk" : dirty ? "unsaved" : "default";

  return (
    <Sheet
      open
      onOpenChange={(open) => (open ? undefined : close())}
      title={t("instructions.title", "Instructions & skills · {{name}}", { name: agent?.name ?? "" })}
      lead={
        dirty
          ? t("instructions.unsaved", "Unsaved changes · ⌘S")
          : t("instructions.saved", "Saved · takes effect from the next session")
      }
      footer={
        <>
          <button type="button" className="btn" onClick={close}>
            {t("instructions.close", "Close")}
          </button>
          <button type="button" className="btn primary" disabled={!dirty || saving} onClick={save}>
            {t("instructions.save", "Save")}
            <kbd>⌘S</kbd>
          </button>
        </>
      }
    >
      <div
        className="instructions"
        data-state={state}
        onKeyDown={(e) => {
          if (e.metaKey && e.key.toLowerCase() === "s") {
            e.preventDefault();
            save();
          }
        }}
      >
        {state === "changed-on-disk" ? (
          <div className="banner info" role="alert">
            <div className="bt">
              <b>{t("instructions.changedTitle", "The file changed in the folder")}</b>
              <span>{t("instructions.changedBody", "AGENTS.md was edited elsewhere.")}</span>
            </div>
            <button type="button" className="btn" onClick={read}>
              {t("instructions.reload", "Reload")}
            </button>
            <button type="button" className="btn" onClick={keepMine}>
              {t("instructions.keepMine", "Keep my edits")}
            </button>
          </div>
        ) : null}
        {error ? (
          <p className="err" role="alert">
            {error}
          </p>
        ) : null}
        <label className="field">
          <span>{INSTRUCTIONS}</span>
          <textarea
            className="textarea mono"
            value={draft}
            disabled={loaded === null}
            onChange={(e) => setDraft(e.target.value)}
          />
        </label>
        <div className="field">
          <span>{t("instructions.skills", { count: loaded?.skills.length ?? 0, defaultValue_one: "{{count}} skill", defaultValue_other: "{{count}} skills" })}</span>
          <ul className="list" aria-label={t("instructions.skillsLabel", "Skills")}>
            {loaded?.skills.map((path) => (
              <li key={path} className="row">
                <span className="mono">{path}</span>
              </li>
            ))}
          </ul>
        </div>
      </div>
    </Sheet>
  );
}
