// Shell-level data for TASK-008: the latest snapshot whenever formationd is
// reachable. TASK-014's store replaces this with snapshot-plus-events.
import { useEffect, useState } from "react";
import { call, onStatus, status } from "./ipc/client";
import type { Snapshot } from "./ipc/generated/Snapshot";

export type ShellData = { connected: boolean; snapshot: Snapshot | null };

export function useSnapshot(): ShellData {
  const [data, setData] = useState<ShellData>({ connected: false, snapshot: null });

  useEffect(() => {
    let alive = true;
    const load = async (connected: boolean) => {
      if (!connected) {
        if (alive) setData((d) => ({ ...d, connected: false }));
        return;
      }
      try {
        const snapshot = await call("snapshot", {});
        if (alive) setData({ connected: true, snapshot });
      } catch {
        if (alive) setData((d) => ({ ...d, connected: false }));
      }
    };
    const unlisten = onStatus((s) => void load(s.connected));
    void status().then((s) => load(s.connected));
    return () => {
      alive = false;
      void unlisten.then((f) => f());
    };
  }, []);

  return data;
}
