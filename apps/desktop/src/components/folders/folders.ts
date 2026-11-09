// Folders picked with the macOS folder picker (Tauri dialog plugin): the agent's
// repositories in SC-004 and SC-020, and an existing agent folder in SC-004.
import { homeDir } from "@tauri-apps/api/path";
import { open } from "@tauri-apps/plugin-dialog";
import { useEffect, useState } from "react";

/** Opens the folder picker; resolves to the chosen folders, or none when canceled. */
export async function pickFolders(multiple: boolean): Promise<string[]> {
  const chosen = await open({ directory: true, multiple });
  if (chosen === null) return [];
  return Array.isArray(chosen) ? chosen : [chosen];
}

/** `list` followed by the folders in `added` it does not hold yet. */
export function addFolders(list: string[], added: string[]): string[] {
  const out = [...list];
  for (const folder of added) if (!out.includes(folder)) out.push(folder);
  return out;
}

/** The path with the home folder shortened to `~`. */
export function tildePath(path: string, home: string | null): string {
  if (!home) return path;
  const base = home.replace(/\/+$/, "");
  if (path === base) return "~";
  return path.startsWith(`${base}/`) ? `~${path.slice(base.length)}` : path;
}

let home: Promise<string | null> | undefined;

/** The home folder, or null until it is known or when it cannot be read. */
export function useHomeDir(): string | null {
  const [value, setValue] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    home ??= homeDir().catch(() => null);
    void home.then((h) => {
      if (live) setValue(h);
    });
    return () => {
      live = false;
    };
  }, []);
  return value;
}
