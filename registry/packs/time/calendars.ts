/**
 * `time` foundation pack (#1591, Astra ruling 2026-11-23).
 *
 * Minimal OWL-Time calendar objects anchored to pinned IANA tzdb rules
 * (Core 01 TIME-PIN-1). Calendar definitions live here as versioned domain
 * objects outside `registry/core/` so non-calendar packs do not inherit them.
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Calendar = Norn.type({
  name: "time.Calendar",
  role: "thing",
  title: "Calendar",
  version: "0.1.0",
  description: "Pinned IANA timezone and DST policy for materializing wall-clock schedules into canonical UTC intervals (TIME-PIN-1).",
  properties: Schema.Struct({
    id: Schema.String,
    code: Norn.ref("core.code"),
    label: Norn.ref("core.display_name"),
    iana_timezone: Schema.String,
    dst_policy: Schema.Literals(["reject", "earlier", "later"]),
    tzdb_version: Norn.ref("core.code"),
    workweek_start: Schema.Literals(["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    dst_policy: "reject",
    workweek_start: "sunday",
  },
})

export const CalendarException = Norn.type({
  name: "time.CalendarException",
  role: "thing",
  title: "Calendar exception",
  version: "0.1.0",
  description: "A public holiday, working-day override, or blackout date attached to a calendar.",
  properties: Schema.Struct({
    id: Schema.String,
    calendar_date: Norn.date(),
    kind: Schema.Literals(["holiday", "working_override", "blackout"]),
    label: Norn.ref("core.display_name"),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "calendar", description: "Calendar this exception belongs to.", target: Calendar, cardinality: { to: "one", from: "many", required: true } },
  ],
})

export const WorkingWindow = Norn.type({
  name: "time.WorkingWindow",
  role: "thing",
  title: "Working window",
  version: "0.1.0",
  description: "Recurring local wall-clock operating window materialized to UTC intervals through checked timezone resolution.",
  properties: Schema.Struct({
    id: Schema.String,
    weekday: Schema.Literals(["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"]),
    start_local: Schema.String,
    end_local: Schema.String,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "calendar", description: "Calendar whose timezone resolves this window.", target: Calendar, cardinality: { to: "one", from: "many", required: true } },
  ],
})
