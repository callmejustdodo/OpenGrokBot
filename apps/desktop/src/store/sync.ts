// Keeps conversations whole and gives every action a pending state.
import type { Method, Params, Result } from "../ipc/generated/commands";
import { appStore, type AppStore } from "./store";
import type { Transport, Unsubscribe } from "./transport";

/** How many recent items a conversation reload asks for. */
export const PAGE_LIMIT = 50;

/**
 * Reloads a conversation with `conversation_page` whenever its stream shows a
 * gap (`staleConversations`), one request per agent at a time.
 */
export function reloadStaleConversations(transport: Transport, store: AppStore = appStore): Unsubscribe {
  const loading = new Set<string>();
  const check = () => {
    const mirror = store.getState().mirror;
    if (!mirror || !store.getState().connected) return;
    for (const agentId of mirror.staleConversations) {
      if (loading.has(agentId)) continue;
      loading.add(agentId);
      transport
        .call("conversation_page", { agent_id: agentId, session_id: null, before: null, limit: PAGE_LIMIT })
        .then((page) => store.getState().loadConversationPage(agentId, page))
        .catch(() => {})
        .finally(() => loading.delete(agentId));
    }
  };
  check();
  return store.subscribe(check);
}

/**
 * Runs a command with a pending state under `key`, so the UI can show progress
 * at once (within 100 ms, frontend architecture "Data flow"). The pending
 * state ends when the reply arrives; the confirming event updates the mirror.
 */
export async function act<M extends Method>(
  transport: Transport,
  key: string,
  method: M,
  params: Params<M>,
  store: AppStore = appStore,
): Promise<Result<M>> {
  store.getState().beginPending(key);
  try {
    return await transport.call(method, params);
  } finally {
    store.getState().endPending(key);
  }
}
