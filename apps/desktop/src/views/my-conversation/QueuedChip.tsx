// SC-005 queued message: one of my messages waiting for the next turn, with
// Steer now (only while a turn runs), Edit (inline), and Withdraw.
import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { QueueItem } from "../../ipc/generated/QueueItem";
import { useAppStore } from "../../store/store";
import { edit, errorText, pendingKey, steer, withdraw } from "./actions";

export default function QueuedChip({ item, turnRunning }: { item: QueueItem; turnRunning: boolean }) {
  const { t } = useTranslation();
  const pending = useAppStore((s) => pendingKey.queue(item.id) in s.pending);
  const [draft, setDraft] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const run = (call: () => Promise<unknown>, after?: () => void) => {
    setError(null);
    call().then(
      () => after?.(),
      (e: unknown) => setError(errorText(t, e)),
    );
  };

  if (draft !== null) {
    const save = () => run(() => edit(item.agent_id, item.id, draft.trim()), () => setDraft(null));
    return (
      <div className="queued editing" data-pending={pending}>
        <textarea
          aria-label={t("conversation.queued.editLabel", "Edit queued message")}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && e.metaKey) {
              e.preventDefault();
              if (draft.trim()) save();
            } else if (e.key === "Escape") {
              e.preventDefault();
              setDraft(null);
            }
          }}
        />
        <button type="button" className="btn primary" disabled={pending || !draft.trim()} onClick={save}>
          {t("conversation.queued.save", "Save")}
        </button>
        <button type="button" className="btn plain" onClick={() => setDraft(null)}>
          {t("conversation.queued.cancelEdit", "Cancel")}
        </button>
        {error ? <span role="alert" className="err">{error}</span> : null}
      </div>
    );
  }

  return (
    <div className="queued" data-pending={pending}>
      <span className="qt">
        “{item.text}”
        <small>{t("conversation.queued.note", "Queued · delivered when this turn ends")}</small>
      </span>
      {turnRunning ? (
        <button
          type="button"
          className="btn primary"
          disabled={pending}
          onClick={() => run(() => steer(item.agent_id, item.id))}
        >
          {t("conversation.queued.steer", "Steer now")}
        </button>
      ) : null}
      <button type="button" className="btn plain" disabled={pending} onClick={() => setDraft(item.text)}>
        {t("conversation.queued.edit", "Edit")}
      </button>
      <button
        type="button"
        className="btn plain"
        disabled={pending}
        onClick={() => run(() => withdraw(item.agent_id, item.id))}
      >
        {t("conversation.queued.withdraw", "Withdraw")}
      </button>
      {error ? <span role="alert" className="err">{error}</span> : null}
    </div>
  );
}
