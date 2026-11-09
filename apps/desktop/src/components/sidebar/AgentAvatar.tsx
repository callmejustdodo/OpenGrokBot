// An agent's avatar slot (D-042). D renders the open-bot-avatar shape and color
// from the data attributes (TASK-016); the badge is a state dot for my agents or
// the owner's initial for teammates' agents.
import type { AgentState } from "../../ipc/generated/AgentState";
import type { Avatar } from "../../ipc/generated/Avatar";

export type AgentAvatarProps = {
  avatar: Avatar;
  size?: number;
  /** A state dot for my agents. */
  state?: AgentState;
  /** The owner's initial for a teammate's agent. */
  ownerInitial?: string;
};

export default function AgentAvatar({ avatar, size = 40, state, ownerInitial }: AgentAvatarProps) {
  return (
    <span
      className="ava"
      aria-hidden="true"
      data-shape={avatar.shape}
      data-color={avatar.color}
      data-size={size}
      data-state={state}
    >
      {ownerInitial ? <span className="owner-initial">{ownerInitial}</span> : null}
    </span>
  );
}

/** A person's avatar slot: their initial. */
export function PersonAvatar({ name, size = 40 }: { name: string; size?: number }) {
  return (
    <span className="ava person" aria-hidden="true" data-size={size}>
      {name.slice(0, 1).toUpperCase()}
    </span>
  );
}
