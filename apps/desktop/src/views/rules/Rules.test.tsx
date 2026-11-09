import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import m3 from "../../../../../tools/fake-formationd/fixtures/worlds/m3.json";
import "../../i18n";
import type { Event } from "../../ipc/generated/Event";
import type { Rule } from "../../ipc/generated/Rule";
import type { Snapshot } from "../../ipc/generated/Snapshot";
import { appStore } from "../../store/store";
import { uiStore } from "../../store/ui";
import ViewRouter from "../ViewRouter";

const { call } = vi.hoisted(() => ({ call: vi.fn() }));
vi.mock("../../ipc/transport", () => ({
  tauriTransport: { call: (...args: unknown[]) => call(...args), onEvent: () => () => {}, onStatus: () => () => {} },
}));

const M3_NOW = new Date("2026-11-09T01:53:00Z");
const MINSU_RULE = "01930000-0000-7000-8000-000000000901";
const JIWOO_RULE = "01930000-0000-7000-8000-000000000902";
const world = m3 as unknown as Snapshot;
const minsuRule = world.rules.find((r) => r.id === MINSU_RULE) as Rule;

function load(rules: Rule[] = world.rules) {
  appStore.setState({ pending: {} });
  appStore.getState().loadSnapshot({ ...world, rules });
}

function apply(event: Event) {
  const mirror = appStore.getState().mirror;
  if (!mirror) throw new Error("load the world first");
  act(() => appStore.getState().receive({ seq: mirror.seq + 1, at: M3_NOW.toISOString(), event }));
}

function showRules() {
  render(<ViewRouter />);
  return screen.getByRole("region", { name: "Rules" });
}

function rows() {
  return within(screen.getByRole("list", { name: "Saved rules" })).getAllByRole("listitem");
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["Date"], now: M3_NOW });
  call.mockReset();
  call.mockResolvedValue({});
  uiStore.setState({ view: { kind: "rules" }, sheet: null });
});

afterEach(() => vi.useRealTimers());

describe("rules page (SC-012)", () => {
  it("default: lists the always-allowed reads first, then each rule's who, what, folder, expiry and uses", () => {
    load();
    const page = showRules();
    expect(page).toHaveAttribute("data-state", "default");
    expect(within(page).getByRole("heading", { name: "Rules" })).toBeInTheDocument();
    expect(page).toHaveTextContent("What runs without asking");
    expect(page).toHaveTextContent("Always allowed");
    expect(page).toHaveTextContent("File reads inside an agent's folder and assigned repositories");

    const [minsu, jiwoo] = rows();
    expect(minsu).toHaveTextContent("Minsu → Backend");
    expect(minsu).toHaveTextContent("npm test -- auth/* · ~/Codes/shared-api");
    expect(minsu).toHaveTextContent("6 days left · used 3×");
    expect(minsu).toHaveAttribute("data-status", "active");
    expect(jiwoo).toHaveTextContent("Jiwoo → Marketing");
    expect(jiwoo).toHaveTextContent("write drafts/* · ~/Formation/Agents/marketing");
    expect(jiwoo).toHaveTextContent("No expiry · used 12×");
    expect(within(minsu).getByRole("button", { name: "Edit limits of the rule Minsu → Backend" })).toBeEnabled();
    expect(within(minsu).getByRole("button", { name: "Revoke the rule Minsu → Backend" })).toBeEnabled();
  });

  it("empty: explains where rules come from", () => {
    load([]);
    const page = showRules();
    expect(page).toHaveAttribute("data-state", "empty");
    expect(page).toHaveTextContent("No saved rules");
    expect(page).toHaveTextContent("Rules appear here when you choose 'Keep allowing similar' while approving.");
    expect(screen.queryByRole("list", { name: "Saved rules" })).not.toBeInTheDocument();
  });

  it("adds a rule saved from the approval sheet's keep-similar choice", () => {
    load([]);
    showRules();
    apply({ type: "rule.update", payload: { rule: { ...minsuRule, id: "rule-new", uses: 1 } } });
    expect(rows()).toHaveLength(1);
    expect(rows()[0]).toHaveTextContent("used 1×");
  });

  it("marks expired and used-up rules, lists them after the ones that apply, and shows limits", () => {
    load([
      { ...minsuRule, id: "expired", expires_at: "2026-11-08T00:00:00Z", created_at: "2026-11-09T00:00:00Z" },
      { ...minsuRule, id: "used-up", expires_at: "2026-11-09T05:00:00Z", max_uses: 5, uses: 5 },
      { ...minsuRule, id: "limited", expires_at: "2026-11-09T05:00:00Z", max_uses: 10, uses: 4 },
    ]);
    showRules();
    const [limited, ...rest] = rows();
    expect(limited).toHaveAttribute("data-status", "active");
    expect(limited).toHaveTextContent("4 hours left · used 4 of 10");
    const expired = rest.find((r) => r.getAttribute("data-status") === "expired");
    const usedUp = rest.find((r) => r.getAttribute("data-status") === "used-up");
    expect(expired).toHaveTextContent("Expired · no longer applies");
    expect(expired).toHaveTextContent("Expired · used 3×");
    expect(usedUp).toHaveTextContent("Limit reached · no longer applies");
    expect(usedUp).toHaveTextContent("used 5 of 5");
  });

  it("shows an allow-all rule as every action, with no folder", () => {
    load([{ ...minsuRule, scope: { kind: "all" } }]);
    showRules();
    expect(rows()[0]).toHaveTextContent("Every action");
    expect(rows()[0]).not.toHaveTextContent("~/Codes/shared-api");
  });
});

describe("revoking a rule", () => {
  it("asks first; Cancel keeps the rule", () => {
    load();
    showRules();
    fireEvent.click(within(rows()[0]).getByRole("button", { name: /^Revoke/ }));
    const alert = screen.getByRole("alertdialog", { name: "Revoke this rule?" });
    expect(alert).toHaveTextContent(
      "Minsu's requests to Backend will ask you again before running npm test -- auth/*. This applies at once.",
    );
    fireEvent.click(within(alert).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(call).not.toHaveBeenCalled();
  });

  it("revokes through rules_revoke, shows pending at once, and drops the rule when formationd confirms", async () => {
    load();
    showRules();
    const reply = deferred<Rule>();
    call.mockReturnValueOnce(reply.promise);
    fireEvent.click(within(rows()[0]).getByRole("button", { name: /^Revoke/ }));
    fireEvent.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "Revoke" }));
    expect(call).toHaveBeenCalledWith("rules_revoke", { rule_id: MINSU_RULE });
    expect(rows()[0]).toHaveAttribute("aria-busy", "true");
    expect(within(rows()[0]).getByRole("button", { name: /^Revoke/ })).toBeDisabled();
    expect(within(rows()[0]).getByRole("button", { name: /^Edit/ })).toBeDisabled();

    const revoked = { ...minsuRule, revoked_at: M3_NOW.toISOString() };
    await act(async () => reply.resolve(revoked));
    apply({ type: "rule.update", payload: { rule: revoked } });
    expect(screen.getByRole("status")).toHaveTextContent("Rule revoked");
    expect(rows()).toHaveLength(1);
    expect(rows()[0]).toHaveTextContent("Jiwoo → Marketing");
  });

  it("shows the empty state once the last rule is revoked", () => {
    load([minsuRule]);
    showRules();
    apply({ type: "rule.update", payload: { rule: { ...minsuRule, revoked_at: M3_NOW.toISOString() } } });
    expect(screen.getByRole("region", { name: "Rules" })).toHaveAttribute("data-state", "empty");
  });

  it("explains a failure and offers to try again", async () => {
    load();
    showRules();
    call.mockRejectedValueOnce({ code: -32000, message: "boom", data: { code: "internal", message: "boom" } });
    fireEvent.click(within(rows()[0]).getByRole("button", { name: /^Revoke/ }));
    await act(async () => fireEvent.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "Revoke" })));
    const banner = screen.getByRole("alert");
    expect(banner).toHaveTextContent("The rule wasn't revoked. It still applies.");
    await act(async () => fireEvent.click(within(banner).getByRole("button", { name: "Try again" })));
    expect(call).toHaveBeenCalledTimes(2);
    expect(call).toHaveBeenLastCalledWith("rules_revoke", { rule_id: MINSU_RULE });
    expect(screen.getByRole("status")).toHaveTextContent("Rule revoked");
  });

  it("says when the rule is already gone, with Dismiss", async () => {
    load();
    showRules();
    call.mockRejectedValueOnce({ data: { code: "not_found", message: "no rule" } });
    fireEvent.click(within(rows()[0]).getByRole("button", { name: /^Revoke/ }));
    await act(async () => fireEvent.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "Revoke" })));
    const banner = screen.getByRole("alert");
    expect(banner).toHaveTextContent("This rule no longer exists.");
    fireEvent.click(within(banner).getByRole("button", { name: "Dismiss" }));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });
});

describe("editing limits", () => {
  function openEditor(index = 0) {
    fireEvent.click(within(rows()[index]).getByRole("button", { name: /^Edit/ }));
    return screen.getByRole("form", { name: "Limits" });
  }

  it("sends both limits through rules_update and closes", async () => {
    load();
    showRules();
    const form = openEditor();
    const expiry = within(form).getByLabelText("Expires");
    expect(expiry).toHaveFocus();
    expect(expiry).toHaveValue("keep");
    fireEvent.change(expiry, { target: { value: "7d" } });
    fireEvent.change(within(form).getByLabelText("Use limit"), { target: { value: "10" } });
    await act(async () => fireEvent.click(within(form).getByRole("button", { name: "Save limits" })));
    expect(call).toHaveBeenCalledWith("rules_update", {
      rule_id: MINSU_RULE,
      expires_at: "2026-11-16T01:53:00.000Z",
      max_uses: 10,
    });
    expect(screen.queryByRole("form", { name: "Limits" })).not.toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Limits saved");
  });

  it("keeps the current expiry and removes limits left blank", async () => {
    load();
    showRules();
    const form = openEditor();
    await act(async () => fireEvent.submit(form));
    expect(call).toHaveBeenCalledWith("rules_update", {
      rule_id: MINSU_RULE,
      expires_at: minsuRule.expires_at,
      max_uses: null,
    });
  });

  it("starts a rule without expiry at No expiry", () => {
    load();
    showRules();
    const form = openEditor(1);
    expect(within(form).getByLabelText("Expires")).toHaveValue("never");
    expect(within(form).queryByRole("option", { name: /^Keep/ })).not.toBeInTheDocument();
    fireEvent.submit(form);
    expect(call).toHaveBeenCalledWith("rules_update", { rule_id: JIWOO_RULE, expires_at: null, max_uses: null });
  });

  it("refuses a use limit at or below the uses so far", () => {
    load();
    showRules();
    const form = openEditor();
    const limit = within(form).getByLabelText("Use limit");
    fireEvent.change(limit, { target: { value: "3" } });
    expect(limit).toHaveAttribute("aria-invalid", "true");
    expect(form).toHaveTextContent("Enter a whole number above 3, the uses so far.");
    expect(within(form).getByRole("button", { name: "Save limits" })).toBeDisabled();
    fireEvent.submit(form);
    expect(call).not.toHaveBeenCalled();
  });

  it("Esc and Cancel close the editor without saving", () => {
    load();
    showRules();
    let form = openEditor();
    fireEvent.keyDown(within(form).getByLabelText("Expires"), { key: "Escape" });
    expect(screen.queryByRole("form", { name: "Limits" })).not.toBeInTheDocument();
    form = openEditor();
    fireEvent.click(within(form).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("form", { name: "Limits" })).not.toBeInTheDocument();
    expect(call).not.toHaveBeenCalled();
  });

  it("keeps the editor open with a retry when saving fails", async () => {
    load();
    showRules();
    call.mockRejectedValueOnce({ data: { code: "disconnected", message: "gone" } });
    const form = openEditor();
    await act(async () => fireEvent.submit(form));
    expect(screen.getByRole("alert")).toHaveTextContent("OpenGrokBot isn't reachable. Try again in a moment.");
    expect(screen.getByRole("form", { name: "Limits" })).toBeInTheDocument();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Try again" })));
    expect(call).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole("form", { name: "Limits" })).not.toBeInTheDocument();
  });
});
