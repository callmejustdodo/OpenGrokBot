// Stands in for src-tauri so the app runs in a plain browser: `@tauri-apps/api`
// calls `window.__TAURI_INTERNALS__.invoke`, which this answers. formationd is
// always connected; the preview loads the store itself, so `snapshot` is never
// needed, and other commands fail the way an unsimulated fake-formationd method does.
import reactions from "../../../../tools/fake-formationd/fixtures/reactions.json";

type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;

/** How `agents_detect` answers: at once, or never (the detecting state). */
let detect: "answer" | "hang" = "answer";

export function setDetect(mode: "answer" | "hang"): void {
  detect = mode;
}

function formationdCall(method: string): Promise<unknown> {
  if (method === "agents_detect") {
    return detect === "hang" ? new Promise(() => {}) : Promise.resolve(reactions.agents_detect[0].result);
  }
  const message = `The preview doesn't simulate ${method}`;
  return Promise.reject({ code: "rpc", message, rpc_code: -32603, data: { code: "internal", message } });
}

export function installTauriMock(target: Window = window): void {
  let callback = 0;
  const invoke: Invoke = async (cmd, args) => {
    if (cmd === "formationd_status") return { connected: true, socket: "preview" };
    if (cmd === "formationd_call") return formationdCall(String(args?.method));
    if (cmd.startsWith("plugin:event")) return ++callback;
    return null;
  };
  Object.assign(target, {
    __TAURI_INTERNALS__: { transformCallback: () => ++callback, invoke },
    __TAURI_EVENT_PLUGIN_INTERNALS__: { unregisterListener: () => {} },
  });
}
