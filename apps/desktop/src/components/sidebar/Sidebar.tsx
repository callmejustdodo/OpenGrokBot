// SC-003 sidebar: top icons, the "+" menu, the New agent button, the search
// field (filters the chat list by name and preview), the Needs-you line, the chat list
// (most recent first), and the profile with Connect agents. States: default,
// empty (no agents yet), detecting (an `agents_detect` call is running).
// Settings (the gear) comes with TASK-035.
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";
import i18n from "../../i18n";
import type { ChatListItem } from "../../store/selectors";
import { selectChatList, selectNeedsYouCount } from "../../store/selectors";
import { useAppStore } from "../../store/store";
import { uiStore, useUi, type View } from "../../store/ui";
import type { MirrorState } from "../../store/reducer";
import AgentAvatar, { PersonAvatar } from "./AgentAvatar";
import { itemLine, stateLabel } from "./labels";
import "./Sidebar.css";
import { inboxNewestFirst, latestAgentQuestion, latestPersonQuestion, memberName } from "./selectors";

const ATTENTION_STATES = new Set(["waiting_for_approval", "stalled", "error", "stopped"]);

function navigate(view: View) {
  uiStore.getState().navigate(view);
}

function isSelected(view: View, item: ChatListItem): boolean {
  if (item.kind === "agent") return view.kind === "agent" && view.agentId === item.agent_id;
  if (item.kind === "teammate_agent")
    return view.kind === "teammate-agent" && view.owner === item.owner && view.agentId === item.agent_id;
  return view.kind === "person" && view.memberId === item.member_id;
}

function timeText(at: string | null): string {
  if (!at) return "";
  return new Date(at).toLocaleTimeString(i18n.language, { hour: "numeric", minute: "2-digit" });
}

interface RowView {
  avatar: ReactNode;
  name: string;
  line: string;
  attention: boolean;
  state: string | undefined;
  target: View;
}

function describeRow(t: TFunction, item: ChatListItem, mirror: MirrorState): RowView {
  if (item.kind === "agent") {
    const agent = item.agent;
    const items = mirror.conversations[agent.id]?.items ?? [];
    const last = items[items.length - 1];
    const attention = ATTENTION_STATES.has(agent.state);
    return {
      attention,
      line: attention ? stateLabel(t, agent.state) : last ? itemLine(t, last) : "",
      avatar: <AgentAvatar avatar={agent.avatar} seed={agent.id} state={agent.state} />,
      name: agent.name,
      state: agent.state,
      target: { kind: "agent", agentId: agent.id },
    };
  }
  if (item.kind === "teammate_agent") {
    const owner = memberName(mirror, item.owner);
    const question = latestAgentQuestion(mirror, item.owner, item.agent_id);
    const answer = question?.answer?.text ?? question?.text ?? "";
    return {
      attention: false,
      line: `${owner} · ${answer}`,
      avatar: item.agent ? (
        <AgentAvatar avatar={item.agent.avatar} seed={item.agent.agent_id} ownerInitial={owner.slice(0, 1).toUpperCase()} />
      ) : (
        <PersonAvatar name={owner} />
      ),
      name: item.agent?.name ?? t("sidebar.unsharedAgent", "Agent no longer shared"),
      state: item.agent?.state,
      target: { kind: "teammate-agent", owner: item.owner, agentId: item.agent_id },
    };
  }
  const question = latestPersonQuestion(mirror, item.member_id);
  const name = item.member?.display_name ?? "";
  return {
    attention: false,
    line: question?.answer?.text ?? question?.text ?? "",
    name,
    avatar: <PersonAvatar name={name} />,
    state: undefined,
    target: { kind: "person", memberId: item.member_id },
  };
}

function ChatRow({ item, row, selected }: { item: ChatListItem; row: RowView; selected: boolean }) {
  const { avatar, name, line, attention, state, target } = row;
  return (
    <li className="chat-item" data-kind={item.kind} data-state={state}>
      <button
        type="button"
        className="chat"
        aria-current={selected ? "true" : undefined}
        onClick={() => navigate(target)}
      >
        {avatar}
        <span className="ct">
          <span className="top">
            <span className="nm">{name}</span>
            <time className="tm" dateTime={item.at ?? undefined}>
              {timeText(item.at)}
            </time>
          </span>
          <span className={attention ? "pv attn" : "pv"}>{line}</span>
        </span>
      </button>
    </li>
  );
}

/** Up and Down move between chat items (NFR-009). */
function moveFocus(event: KeyboardEvent<HTMLElement>) {
  if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
  const buttons = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>("button.chat"));
  const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
  if (index === -1) return;
  const next = buttons[index + (event.key === "ArrowDown" ? 1 : -1)];
  if (next) {
    event.preventDefault();
    next.focus();
  }
}

export default function Sidebar() {
  const { t } = useTranslation();
  const mirror = useAppStore((s) => s.mirror);
  const detecting = useAppStore((s) => "agents_detect" in s.pending);
  const view = useUi((s) => s.view);
  const ui = uiStore.getState();

  const [query, setQuery] = useState("");
  const searchRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    // ⌘K focuses the search field from anywhere in the window.
    function onKey(event: globalThis.KeyboardEvent) {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const needle = query.trim().toLocaleLowerCase();
  const rows = mirror
    ? selectChatList(mirror)
        .map((item) => ({ item, row: describeRow(t, item, mirror) }))
        // A conversation with someone no longer on the team (or before joining one)
        // has no name to show.
        .filter(({ item, row }) => item.kind !== "person" || row.name !== "")
        .filter(
          ({ row }) =>
            !needle ||
            row.name.toLocaleLowerCase().includes(needle) ||
            row.line.toLocaleLowerCase().includes(needle),
        )
    : [];
  const needsYou = mirror ? selectNeedsYouCount(mirror) : 0;
  const inbox = mirror ? inboxNewestFirst(mirror) : [];
  const empty = mirror !== null && Object.keys(mirror.agents).length === 0 && !detecting;
  const me = mirror?.team?.members.find((m) => m.id === mirror.team?.me) ?? null;

  const top: { view: View; label: string; count?: number }[] = [
    { view: { kind: "inbox" }, label: t("sidebar.inbox", "Inbox"), count: inbox.length },
    { view: { kind: "team" }, label: t("sidebar.team", "Team") },
    { view: { kind: "history" }, label: t("sidebar.history", "History") },
    { view: { kind: "rules" }, label: t("sidebar.rules", "Rules") },
  ];
  const currentTop = view.kind === "member" ? "team" : view.kind;

  return (
    <aside className="side" role="navigation" aria-label={t("sidebar.label", "Sidebar")} data-state={empty ? "empty" : detecting ? "detecting" : "default"}>
      <div className="side-top" data-tauri-drag-region>
        {top.map((icon) => (
          <button
            key={icon.view.kind}
            type="button"
            className="ticon"
            data-icon={icon.view.kind}
            aria-current={currentTop === icon.view.kind ? "page" : undefined}
            aria-label={icon.count ? t("sidebar.withCount", "{{label}} ({{count}})", { label: icon.label, count: icon.count }) : icon.label}
            title={icon.label}
            onClick={() => navigate(icon.view)}
          >
            {icon.count ? <span className="cnt">{icon.count}</span> : null}
          </button>
        ))}
        <DropdownMenu.Root>
          <DropdownMenu.Trigger asChild>
            <button type="button" className="ticon plus" aria-label={t("sidebar.new", "New")} title={t("sidebar.new", "New")} />
          </DropdownMenu.Trigger>
          <DropdownMenu.Portal>
            <DropdownMenu.Content className="pop" align="start">
              <DropdownMenu.Item onSelect={() => ui.openSheet({ kind: "new-agent" })}>
                {t("sidebar.newAgent", "New agent")}
                <kbd>⇧⌘N</kbd>
              </DropdownMenu.Item>
              <DropdownMenu.Item onSelect={() => navigate({ kind: "new-question" })}>
                {t("sidebar.newQuestion", "New question")}
                <kbd>⌘N</kbd>
              </DropdownMenu.Item>
            </DropdownMenu.Content>
          </DropdownMenu.Portal>
        </DropdownMenu.Root>
      </div>

      {/* The empty state carries its own New agent button. */}
      {empty ? null : (
        <button type="button" className="new-agent" onClick={() => ui.openSheet({ kind: "new-agent" })}>
          {t("sidebar.newAgent", "New agent")}
        </button>
      )}

      <label className="side-search">
        <input
          ref={searchRef}
          type="search"
          value={query}
          placeholder={t("sidebar.search", "Search agents and chats")}
          aria-label={t("sidebar.search", "Search agents and chats")}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              setQuery("");
            }
          }}
        />
        <kbd aria-hidden="true">⌘K</kbd>
      </label>

      {needsYou > 0 ? (
        <button type="button" className="needs" onClick={() => navigate({ kind: "inbox" })}>
          <b>{t("sidebar.needsYou", "{{count}} need you", { count: needsYou })}</b>
          {inbox.length > 0 ? (
            <small>
              {inbox
                .slice(0, 2)
                .map((i) => i.subject)
                .join(" · ")}
            </small>
          ) : null}
        </button>
      ) : null}

      <nav className="chats" aria-label={t("sidebar.conversations", "Conversations")} onKeyDown={moveFocus}>
        {rows.length > 0 ? (
          <>
            <h2 className="side-group">{t("sidebar.myAgents", "My agents")}</h2>
            <ul className="chat-list">
              {rows.map(({ item, row }) => (
                <ChatRow
                  key={item.kind === "person" ? item.member_id : `${item.kind}/${item.agent_id}`}
                  item={item}
                  row={row}
                  selected={isSelected(view, item)}
                />
              ))}
            </ul>
          </>
        ) : null}
        {needle && rows.length === 0 ? (
          <p className="side-nomatch" role="status">
            {t("sidebar.noMatches", "No matches")}
          </p>
        ) : null}
        {detecting ? (
          <div className="chat detecting" role="status">
            <span className="dots" aria-hidden="true">
              <i />
              <i />
              <i />
            </span>
            <span className="muted small">{t("sidebar.detecting", "Looking for agents on this Mac…")}</span>
          </div>
        ) : null}
        {empty ? (
          <div className="empty">
            <b>{t("sidebar.empty.title", "Create your first agent")}</b>
            <span>{t("sidebar.empty.body", "Pick a role, or connect agents already running.")}</span>
            <div className="row-actions">
              <button type="button" className="btn primary" onClick={() => ui.openSheet({ kind: "new-agent" })}>
                {t("sidebar.newAgent", "New agent")}
              </button>
              <button type="button" className="btn" onClick={() => ui.openSheet({ kind: "connect" })}>
                {t("sidebar.connectRunning", "Connect agents already running")}
              </button>
            </div>
          </div>
        ) : null}
      </nav>

      <div className="side-bottom">
        {me ? (
          <span className="prof" data-online={me.online}>
            <PersonAvatar name={me.display_name} size={32} />
            <span>
              {me.display_name}
              <small>{me.online ? t("sidebar.online", "Online") : t("sidebar.offline", "Offline")}</small>
            </span>
          </span>
        ) : null}
        <button type="button" className="btn" onClick={() => ui.openSheet({ kind: "connect" })}>
          {t("sidebar.connect", "Connect agents")}
        </button>
      </div>
    </aside>
  );
}
