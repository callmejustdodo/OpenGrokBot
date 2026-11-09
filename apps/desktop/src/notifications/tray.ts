// The menu bar icon's dot (SC-002): the menu bar panel's window tells
// `src-tauri` whether anything needs the owner, and it swaps the tray icon.
import { invoke } from "@tauri-apps/api/core";

export function setTrayAttention(attention: boolean): Promise<void> {
  return invoke<void>("set_tray_attention", { attention }).catch(() => {});
}
