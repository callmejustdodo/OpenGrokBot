// SC-018 "harnesses": where Claude Code and Codex are and whether they are
// signed in, with what to do when one is not ready, and "Check again".
import type { TFunction } from "i18next";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { HarnessKind } from "../../ipc/generated/HarnessKind";
import type { HarnessStatus } from "../../ipc/generated/HarnessStatus";
import { tauriTransport } from "../../ipc/transport";
import { useAppStore } from "../../store/store";
import { act } from "../../store/sync";

const CHECK_PENDING = "harness_check";
const NO_HARNESSES: readonly HarnessStatus[] = [];

type HarnessState = "not-installed" | "signed-in" | "sign-in-needed" | "sign-in-unknown";

function harnessState(h: HarnessStatus): HarnessState {
  if (!h.installed) return "not-installed";
  if (h.signed_in === null) return "sign-in-unknown";
  return h.signed_in ? "signed-in" : "sign-in-needed";
}

function harnessName(t: TFunction, kind: HarnessKind): string {
  switch (kind) {
    case "claude_code":
      return t("harness.claude_code", "Claude Code");
    case "codex":
      return t("harness.codex", "Codex");
    case "detected":
      return t("settings.harnesses.detected", "Detected agent");
  }
}

function stateLabel(t: TFunction, state: HarnessState): string {
  switch (state) {
    case "not-installed":
      return t("settings.harnesses.notInstalled", "Not installed");
    case "signed-in":
      return t("settings.harnesses.signedIn", "Signed in");
    case "sign-in-needed":
      return t("settings.harnesses.signInNeeded", "Sign-in needed");
    case "sign-in-unknown":
      return t("settings.harnesses.signInUnknown", "Sign-in status unknown");
  }
}

export default function HarnessesTab() {
  const { t } = useTranslation();
  const harnesses = useAppStore((s) => s.mirror?.harnesses ?? NO_HARNESSES);
  const connected = useAppStore((s) => s.connected);
  const checking = useAppStore((s) => CHECK_PENDING in s.pending);
  const [failed, setFailed] = useState(false);

  const check = () => {
    setFailed(false);
    act(tauriTransport, CHECK_PENDING, "harness_check", {}).catch(() => setFailed(true));
  };

  return (
    <>
      <ul className="list">
        {harnesses.map((h) => {
          const state = harnessState(h);
          return (
            <li key={h.harness} className="form-row" data-state={state}>
              <span>
                <b>{harnessName(t, h.harness)}</b>
                <small className="mono">
                  {h.path ?? t("settings.harnesses.noPath", "Not found")}
                  {h.version ? ` · ${h.version}` : ""}
                </small>
              </span>
              <span className={state === "signed-in" ? "ok" : "bad"}>{stateLabel(t, state)}</span>
              {h.problem ? <p className="err">{h.problem}</p> : null}
            </li>
          );
        })}
      </ul>
      <p className="muted small">
        {t("settings.harnesses.note", "OpenGrokBot never handles sign-in. Sign in from each harness.")}
      </p>
      <button type="button" className="btn" onClick={check} disabled={checking || !connected} aria-busy={checking}>
        {checking ? t("settings.harnesses.checking", "Checking…") : t("settings.harnesses.check", "Check again")}
      </button>
      {failed ? (
        <p className="err" role="alert">
          {t("settings.harnesses.checkFailed", "Couldn't check the harnesses. Try again.")}
        </p>
      ) : null}
    </>
  );
}
