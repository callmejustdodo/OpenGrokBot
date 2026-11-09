// An agent's avatar slot (D-042): the open-bot-avatar shape in its color, drawn
// as a flat still like the mockup (one body color, two dark eyes); a rounded
// square in the color when the shape is unknown. The badge is a state dot for my
// agents (drawn by CSS from `data-state`) or the owner's initial for teammates' agents.
import type { CSSProperties } from "react";
import type { AgentState } from "../../ipc/generated/AgentState";
import type { Avatar } from "../../ipc/generated/Avatar";
import { BOT_SHAPES } from "./avatarShapes";

export type AgentAvatarProps = {
  avatar: Avatar;
  size?: number;
  /** A state dot for my agents. */
  state?: AgentState;
  /** The owner's initial for a teammate's agent. */
  ownerInitial?: string;
};

/** The shape as an SVG, or null for an unknown shape. */
export function BotShapeSvg({ shape, color }: { shape: string; color: string }) {
  const s = BOT_SHAPES[shape];
  if (!s) return null;
  const [x, y, spacing, scale] = s.e;
  const w = 4 * scale * 1.25;
  const h = 11 * scale * 1.25;
  const eye = (cx: number) => (
    <rect x={cx - w / 2} y={y - h / 2} width={w} height={h} rx={w / 2} fill="var(--eye)" />
  );
  return (
    <svg viewBox="0 0 100 100" aria-hidden="true">
      <path d={s.p} fill={color} />
      {eye(x - spacing / 2)}
      {eye(x + spacing / 2)}
    </svg>
  );
}

export default function AgentAvatar({ avatar, size = 40, state, ownerInitial }: AgentAvatarProps) {
  const known = avatar.shape in BOT_SHAPES;
  return (
    <span
      className={known ? "ava" : "ava sq"}
      aria-hidden="true"
      data-shape={avatar.shape}
      data-color={avatar.color}
      data-size={size}
      data-state={state}
      style={{ "--size": `${size}px`, "--c": avatar.color } as CSSProperties}
    >
      <BotShapeSvg shape={avatar.shape} color={avatar.color} />
      {ownerInitial ? <span className="owner-initial">{ownerInitial}</span> : null}
    </span>
  );
}

/** A person's avatar slot: their initial, on one of the people colors. */
export function PersonAvatar({ name, size = 40 }: { name: string; size?: number }) {
  let hash = 0;
  for (const ch of name) hash = (hash * 31 + ch.codePointAt(0)!) | 0;
  const tone = Math.abs(hash) % 4;
  return (
    <span
      className="ava person"
      aria-hidden="true"
      data-size={size}
      style={{ "--size": `${size}px`, "--c": `var(--person-${tone})` } as CSSProperties}
    >
      {name.slice(0, 1).toUpperCase()}
    </span>
  );
}
