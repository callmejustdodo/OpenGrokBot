import type { Method, Params, Result } from "../ipc/generated/commands";
import type { EventEnvelope } from "../ipc/generated/EventEnvelope";

export type ConnectionStatus = "connected" | "disconnected";

export type Unsubscribe = () => void;

/** The connection to formationd the store needs; `src/ipc/client.ts` provides it over Tauri. */
export interface Transport {
  call<M extends Method>(method: M, params: Params<M>): Promise<Result<M>>;
  onEvent(callback: (envelope: EventEnvelope) => void): Unsubscribe;
  onStatus(callback: (status: ConnectionStatus) => void): Unsubscribe;
}
