// Starts mirroring formationd once per window.
import { tauriTransport } from "../ipc/transport";
import { connectStore } from "./store";
import { reloadStaleConversations } from "./sync";

let started = false;

export function startStore(): void {
  if (started) return;
  started = true;
  connectStore(tauriTransport);
  reloadStaleConversations(tauriTransport);
}
