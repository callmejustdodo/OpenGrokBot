import { describe, expect, expectTypeOf, it } from "vitest";
import {
  EVENT_NOTIFICATION,
  type Method,
  type Params,
  type Result,
} from "./generated/commands";
import type { EventEnvelope } from "./generated/EventEnvelope";
import type { Snapshot } from "./generated/Snapshot";
import type { SnapshotGet } from "./generated/SnapshotGet";

// The type assertions run in `pnpm typecheck` (tsc); vitest only runs the rest.
describe("generated IPC contract", () => {
  it("maps each method to its params and result", () => {
    expectTypeOf<Params<"snapshot">>().toEqualTypeOf<SnapshotGet>();
    expectTypeOf<Result<"snapshot">>().toEqualTypeOf<Snapshot>();
    expectTypeOf<"agent_start">().toExtend<Method>();
    expectTypeOf<"not_a_method">().not.toExtend<Method>();
  });

  it("names the event notification", () => {
    expect(EVENT_NOTIFICATION).toBe("event");
    expectTypeOf<EventEnvelope["event"]["type"]>().toExtend<string>();
  });
});
