// An agent's avatar slot (D-042): the open-bot-avatar shape in its color, drawn
// as a flat still like the mockup (one body color, two dark eyes); an unknown
// shape is drawn as a set shape chosen from the seed. The badge is a state dot for my
// agents (drawn by CSS from `data-state`) or the owner's initial for teammates' agents.
// A working agent's avatar moves gently and blinks (`data-motion="working"`, CSS in
// styles/components.css); Reduce Motion stops it.
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
  /** Whether the agent is working, for an avatar without a state dot (the pill); defaults to `state === "working"`. */
  working?: boolean;
  /** Picks a steady shape when `avatar.shape` is not one of open-bot-avatar's; pass the agent's ID or name. */
  seed?: string;
};

/** The shape as an SVG, or null for an unknown shape. */
export function BotShapeSvg({ shape, color }: { shape: string; color: string }) {
  const s = BOT_SHAPES[shape];
  if (!s) return null;
  const [x, y, spacing, scale] = s.e;
  const w = 4 * scale * 1.25;
  const h = 11 * scale * 1.25;
  const eye = (cx: number) => (
    <rect x={cx - w / 2} y={y - h / 2} width={w} height={h} rx={w / 2} fill="var(--eye)" className="eye" />
  );
  return (
    <svg viewBox="0 0 100 100" aria-hidden="true">
      <path d={s.p} fill={color} />
      {eye(x - spacing / 2)}
      {eye(x + spacing / 2)}
    </svg>
  );
}

const SHAPE_NAMES = Object.keys(BOT_SHAPES);

/** Plain color names formationd may send (a detected agent gets "blue"), in the avatar palette. */
const NAMED_COLORS: Record<string, string> = {
  orange: "#ff6a1a",
  yellow: "#f8a01c",
  blue: "#3a6df0",
  teal: "#14b3a5",
  green: "#1fbf68",
  purple: "#8a5cf0",
  violet: "#8a5cf0",
  pink: "#ec4899",
  gray: "#7a7a7a",
  grey: "#7a7a7a",
};

function hash(text: string): number {
  let h = 0;
  for (const ch of text) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
  return h;
}

/**
 * The avatar to draw. A shape outside open-bot-avatar's set (formationd gives agents
 * found elsewhere "round") becomes a set shape picked steadily from `seed`, so every
 * agent shows a character rather than a blank square.
 */
function resolveAvatar(avatar: Avatar, seed = ""): Avatar {
  const shape = avatar.shape in BOT_SHAPES ? avatar.shape : SHAPE_NAMES[hash(`${seed}|${avatar.shape}`) % SHAPE_NAMES.length];
  const color = NAMED_COLORS[avatar.color.toLowerCase()] ?? avatar.color;
  return { shape, color };
}

export default function AgentAvatar({ avatar, size = 40, state, ownerInitial, working = state === "working", seed }: AgentAvatarProps) {
  const shown = resolveAvatar(avatar, seed);
  return (
    <span
      className="ava"
      aria-hidden="true"
      data-shape={shown.shape}
      data-color={shown.color}
      data-size={size}
      data-state={state}
      data-motion={working ? "working" : undefined}
      style={{ "--size": `${size}px`, "--c": shown.color } as CSSProperties}
    >
      <BotShapeSvg shape={shown.shape} color={shown.color} />
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
