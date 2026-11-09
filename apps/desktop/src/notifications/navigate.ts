// The main window's side of "open this item": the menu bar panel and
// notification clicks ask `src-tauri` to focus the main window, which emits
// `app://navigate` with an `AttentionTarget`; this turns it into a view and a sheet.
import { listen } from "@tauri-apps/api/event";
import type { AttentionTarget } from "../ipc/generated/AttentionTarget";
import type { MirrorState } from "../store/reducer";
import { appStore, type AppStore } from "../store/store";
import type { Unsubscribe } from "../store/transport";
import { uiStore, type Sheet, type View } from "../store/ui";

export const NAVIGATE = "app://navigate";

export type Destination = { view: View | null; sheet: Sheet | null };

/** Where an item opens: its conversation or the Inbox, and the sheet on top, if any. */
export function destinationFor(target: AttentionTarget, mirror: MirrorState | null): Destination {
  switch (target.kind) {
    case "agent":
      return { view: { kind: "agent", agentId: target.agent_id }, sheet: null };
    case "approval": {
      const approval = mirror?.approvals[target.approval_id];
      // The owner's own permission prompts are answered in the conversation, not the sheet.
      if (approval?.request_id === null) return { view: { kind: "agent", agentId: approval.agent_id }, sheet: null };
      return {
        view: approval ? { kind: "agent", agentId: approval.agent_id } : null,
        sheet: { kind: "approval", approvalId: target.approval_id },
      };
    }
    case "question": {
      const question = mirror?.questions[target.question_id];
      if (!question || question.asker !== mirror?.team?.me) return { view: { kind: "inbox" }, sheet: null };
      const asked = question.target;
      return {
        view:
          asked.kind === "agent"
            ? { kind: "teammate-agent", owner: asked.member_id, agentId: asked.agent_id }
            : { kind: "person", memberId: asked.member_id },
        sheet: null,
      };
    }
    case "request":
      return { view: null, sheet: { kind: "request-status", requestId: target.request_id } };
    case "schedule": {
      const schedule = mirror?.schedules[target.schedule_id];
      if (!schedule) return { view: { kind: "inbox" }, sheet: null };
      return {
        view: { kind: "agent", agentId: schedule.agent_id },
        sheet: { kind: "schedules", agentId: schedule.agent_id },
      };
    }
  }
}

export function openTarget(target: AttentionTarget, store: AppStore = appStore): void {
  const { view, sheet } = destinationFor(target, store.getState().mirror);
  const ui = uiStore.getState();
  if (view) ui.navigate(view);
  if (sheet) ui.openSheet(sheet);
  else ui.closeSheet();
}

/** Listens for `app://navigate` in the main window. */
export function startNavigateListener(store: AppStore = appStore): Unsubscribe {
  let stopped = false;
  let off: (() => void) | null = null;
  void listen<AttentionTarget>(NAVIGATE, (e) => openTarget(e.payload, store)).then((stop) =>
    stopped ? stop() : (off = stop),
  );
  return () => {
    stopped = true;
    off?.();
  };
}
