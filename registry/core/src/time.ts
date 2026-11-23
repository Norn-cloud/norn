/**
 * Core temporal interval interface (`core.TimeInterval`, #1584).
 *
 * Half-open `[starts_at, ends_at)` in UTC with an optional canonical IANA
 * timezone label. Any domain happening (a scheduled shift, a maintenance
 * window, an equipment reservation, a contract period) may implement
 * `core.TimeInterval` and inherit the proper-interval invariant.
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const TimeIntervalProperties = Schema.Struct({
  starts_at: Norn.ref("core.time.instant"),
  ends_at: Norn.ref("core.time.instant"),
  timezone: Schema.NullOr(Norn.ref("core.time.iana_timezone")),
})

const TimeIntervalBounds = Schema.Struct({
  starts_at: Norn.timestamp("utc"),
  ends_at: Norn.timestamp("utc"),
})

export const TimeInterval = Norn.interfaceType({
  name: "core.TimeInterval",
  role: "happening",
  title: "Time interval",
  version: "0.1.0",
  description: "A proper half-open temporal interval [starts_at, ends_at) in UTC with an optional canonical IANA timezone label.",
  properties: TimeIntervalProperties,
  invariants: [
    {
      id: "proper_interval",
      message: "starts_at must be strictly before ends_at",
      expression: Norn.expression(TimeIntervalBounds, (self) => Norn.timestampBefore(self.starts_at, self.ends_at)),
    },
  ],
})
