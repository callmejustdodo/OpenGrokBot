// The UI's typed door to formationd. `src-tauri` keeps the one socket
// connection; this module calls it and listens to what it forwards.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { Method, Params, Result } from "./generated/commands";
import type { EventEnvelope } from "./generated/EventEnvelope";

/** Why a call failed. For `rpc`, `data` is formationd's `ApiError` when set. */
export type CallError = {
  code: "disconnected" | "timeout" | "rpc";
  message: string;
  rpc_code: number | null;
  data: { code: string; message: string } | null;
};

export type Status = { connected: boolean; socket: string };

export const EVENT = "formationd://event";
export const STATUS = "formationd://status";

/** Calls a formationd method with checked params and result. */
export function call<M extends Method>(method: M, params: Params<M>): Promise<Result<M>> {
  return invoke<Result<M>>("formationd_call", { method, params });
}

export function status(): Promise<Status> {
  return invoke<Status>("formationd_status");
}

export function onEvent(handler: (envelope: EventEnvelope) => void): Promise<UnlistenFn> {
  return listen<EventEnvelope>(EVENT, (e) => handler(e.payload));
}

export function onStatus(handler: (status: Status) => void): Promise<UnlistenFn> {
  return listen<Status>(STATUS, (e) => handler(e.payload));
}

export function showMainWindow(): Promise<void> {
  return invoke("show_main_window");
}

/** The transport the store uses (`store/transport.ts` describes the same shape). */
export const transport = { call, status, onEvent, onStatus };
