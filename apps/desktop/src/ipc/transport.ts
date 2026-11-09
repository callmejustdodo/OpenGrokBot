// The store's `Transport` over Tauri: calls go through `src-tauri`'s one
// formationd connection, events and status come back as Tauri events.
import type { Transport, Unsubscribe } from "../store/transport";
import { call, onEvent, onStatus, status } from "./client";

/** Turns an async Tauri `listen` into the store's synchronous unsubscribe. */
function subscribe(listening: Promise<() => void>): Unsubscribe {
  let stopped = false;
  let off: (() => void) | null = null;
  void listening.then((stop) => (stopped ? stop() : (off = stop)));
  return () => {
    stopped = true;
    off?.();
  };
}

export const tauriTransport: Transport = {
  call,
  onEvent: (callback) => subscribe(onEvent(callback)),
  onStatus: (callback) => {
    // Report the current status once, then every change.
    let live = true;
    void status().then((s) => live && callback(s.connected ? "connected" : "disconnected"));
    const off = subscribe(onStatus((s) => callback(s.connected ? "connected" : "disconnected")));
    return () => {
      live = false;
      off();
    };
  },
};
