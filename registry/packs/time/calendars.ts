/**
 * `time` foundation pack (#1591, Astra ruling 2026-11-23).
 *
 * Minimal OWL-Time calendar objects anchored to pinned IANA tzdb rules
 * (Core 01 TIME-PIN-1) and checked UTC interval materialization via
 * `core.TimeInterval` and `resolve_local_timestamp` (`norn-zen/7`).
 * Calendar definitions live here as versioned domain objects outside
 * `registry/core/` so non-calendar packs do not inherit them.
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { TimeInterval } from "../../core/src/time.ts"

export const Calendar = Norn.type({
  name: "time.Calendar",
  role: "thing",
  title: "Calendar",
  version: "0.1.0",
  description: "Pinned IANA timezone and DST policy for materializing wall-clock schedules into canonical UTC intervals (TIME-PIN-1).",
  properties: Schema.Struct({
    id: Schema.String,
    code: Norn.ref<Schema.String>("core.code"),
    label: Norn.ref<Schema.String>("core.display_name"),
    iana_timezone: Norn.ref<Schema.String>("core.time.iana_timezone"),
    dst_policy: Norn.ref<Schema.String>("core.time.dst_policy"),
    tzdb_version: Schema.Literals(["2025b"]),
    workweek_start: Schema.Literals(["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    dst_policy: "reject",
    tzdb_version: "2025b",
    workweek_start: "sunday",
  },
})

export const calendarTzdbVersionPinned = Norn.law("tzdb_version_pinned", {
  on: Calendar,
  must: (self) => self.tzdb_version === "2025b",
  because: "Calendars must pin the exact supported IANA tzdb 2025b release (TIME-PIN-1).",
})

export const calendarTzdbPinnedHolds = Norn.example("calendar with pinned tzdb 2025b holds", {
  of: calendarTzdbVersionPinned,
  given: { id: "cal-ny", code: "US-NY", label: "New York", iana_timezone: "America/New_York", dst_policy: "reject", tzdb_version: "2025b", workweek_start: "monday" },
  expect: "holds",
})

export const calendarTzdbUnpinnedViolated = Norn.example("calendar with unsupported tzdb version is refused", {
  of: calendarTzdbVersionPinned,
  given: { id: "cal-old", code: "US-NY-OLD", label: "New York", iana_timezone: "America/New_York", dst_policy: "reject", tzdb_version: "2024a", workweek_start: "monday" },
  expect: "violated",
})

export const CalendarException = Norn.type({
  name: "time.CalendarException",
  role: "thing",
  title: "Calendar exception",
  version: "0.1.0",
  description: "A public holiday, working-day override, or blackout date attached to a calendar.",
  properties: Schema.Struct({
    id: Schema.String,
    calendar_date: Norn.ref("core.time.date"),
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
  description: "Recurring local wall-clock operating window (HH:MM or HH:MM:SS) materialized to UTC intervals through checked timezone resolution.",
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

export const workingWindowValidLocalTimes = Norn.law("valid_local_window", {
  on: WorkingWindow,
  must: (self) =>
    [5, 8].includes(Norn.byteLength(self.start_local)) &&
    [5, 8].includes(Norn.byteLength(self.end_local)) &&
    Norn.timestampBefore(
      Norn.resolveLocalTimestamp("2026-01-01T" + Norn.take(Norn.take(self.start_local, 8) + ":00", 8), "UTC", "reject"),
      Norn.resolveLocalTimestamp("2026-01-01T" + Norn.take(Norn.take(self.end_local, 8) + ":00", 8), "UTC", "reject"),
    ),
  because: "WorkingWindow start_local and end_local must be strict HH:MM or HH:MM:SS wall times with start_local strictly before end_local.",
})

export const workingWindowHhMmHolds = Norn.example("working window with HH:MM bounds holds", {
  of: workingWindowValidLocalTimes,
  given: { id: "ww-1", weekday: "monday", start_local: "09:00", end_local: "17:00" },
  expect: "holds",
})

export const workingWindowHhMmSsHolds = Norn.example("working window with HH:MM:SS bounds holds", {
  of: workingWindowValidLocalTimes,
  given: { id: "ww-2", weekday: "tuesday", start_local: "08:30:00", end_local: "16:45:00" },
  expect: "holds",
})

export const workingWindowReversedViolated = Norn.example("working window with reversed bounds is refused", {
  of: workingWindowValidLocalTimes,
  given: { id: "ww-3", weekday: "wednesday", start_local: "17:00", end_local: "09:00" },
  expect: "violated",
})

export const workingWindowEqualViolated = Norn.example("working window with equal bounds is refused", {
  of: workingWindowValidLocalTimes,
  given: { id: "ww-4", weekday: "thursday", start_local: "09:00", end_local: "09:00" },
  expect: "violated",
})

export const workingWindowMalformedLengthViolated = Norn.example("working window with non-HH:MM format is refused", {
  of: workingWindowValidLocalTimes,
  given: { id: "ww-5", weekday: "friday", start_local: "9:00", end_local: "17:00" },
  expect: "violated",
})

export const workingWindowOffsetSuffixViolated = Norn.example("working window with timezone suffix is refused", {
  of: workingWindowValidLocalTimes,
  given: { id: "ww-6", weekday: "friday", start_local: "09:00:00Z", end_local: "17:00:00" },
  expect: "violated",
})

export const MaterializedWindow = Norn.type({
  name: "time.MaterializedWindow",
  role: "happening",
  title: "Materialized window",
  version: "0.1.0",
  description: "A half-open UTC interval [starts_at, ends_at) materialized from a WorkingWindow and Calendar under pinned tzdb 2025b and explicit DST policy (implements core.TimeInterval).",
  implements: [TimeInterval],
  properties: Schema.Struct({
    id: Schema.String,
    calendar_date: Norn.ref<Schema.String>("core.time.date"),
    start_local_at: Schema.String,
    end_local_at: Schema.String,
    starts_at: Norn.ref<Schema.String>("core.time.instant"),
    ends_at: Norn.ref<Schema.String>("core.time.instant"),
    timezone: Norn.ref<Schema.String>("core.time.iana_timezone"),
    dst_policy: Norn.ref<Schema.String>("core.time.dst_policy"),
    tzdb_version: Schema.Literals(["2025b"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    dst_policy: "reject",
    tzdb_version: "2025b",
  },
  links: [
    { id: "calendar", description: "Calendar whose pinned timezone and DST policy materialized this interval.", target: Calendar, cardinality: { to: "one", from: "many", required: true } },
    { id: "working_window", description: "Recurring working window materialized into this UTC interval.", target: WorkingWindow, cardinality: { to: "one", from: "many", required: true } },
  ],
})

export const materializedWindowResolvedBounds = Norn.law("resolved_matches_bounds", {
  on: MaterializedWindow,
  must: (self) =>
    self.tzdb_version === "2025b" &&
    Norn.parseTimestamp(self.starts_at) === Norn.resolveLocalTimestamp(self.start_local_at, self.timezone, self.dst_policy) &&
    Norn.parseTimestamp(self.ends_at) === Norn.resolveLocalTimestamp(self.end_local_at, self.timezone, self.dst_policy) &&
    Norn.timestampBefore(self.starts_at, self.ends_at),
  because: "MaterializedWindow UTC starts_at and ends_at must equal the checked tzdb 2025b resolution of start_local_at and end_local_at under the pinned timezone and dst_policy, forming a proper interval.",
})

export const materializedWindowStandardHolds = Norn.example("materialized window on unambiguous EDT day holds", {
  of: materializedWindowResolvedBounds,
  given: {
    id: "mw-1",
    calendar_date: "2026-06-15",
    start_local_at: "2026-06-15T09:00:00",
    end_local_at: "2026-06-15T17:00:00",
    starts_at: "2026-06-15T13:00:00Z",
    ends_at: "2026-06-15T21:00:00Z",
    timezone: "America/New_York",
    dst_policy: "reject",
    tzdb_version: "2025b",
  },
  expect: "holds",
})

export const materializedWindowDstFoldEarlierHolds = Norn.example("materialized window on DST fall-back fold with earlier policy holds", {
  of: materializedWindowResolvedBounds,
  given: {
    id: "mw-2",
    calendar_date: "2026-11-01",
    start_local_at: "2026-11-01T01:30:00",
    end_local_at: "2026-11-01T09:30:00",
    starts_at: "2026-11-01T05:30:00Z",
    ends_at: "2026-11-01T14:30:00Z",
    timezone: "America/New_York",
    dst_policy: "earlier",
    tzdb_version: "2025b",
  },
  expect: "holds",
})

export const materializedWindowDstFoldLaterHolds = Norn.example("materialized window on DST fall-back fold with later policy holds", {
  of: materializedWindowResolvedBounds,
  given: {
    id: "mw-3",
    calendar_date: "2026-11-01",
    start_local_at: "2026-11-01T01:30:00",
    end_local_at: "2026-11-01T09:30:00",
    starts_at: "2026-11-01T06:30:00Z",
    ends_at: "2026-11-01T14:30:00Z",
    timezone: "America/New_York",
    dst_policy: "later",
    tzdb_version: "2025b",
  },
  expect: "holds",
})

export const materializedWindowWrongUtcViolated = Norn.example("materialized window with mismatched UTC starts_at is refused", {
  of: materializedWindowResolvedBounds,
  given: {
    id: "mw-4",
    calendar_date: "2026-06-15",
    start_local_at: "2026-06-15T09:00:00",
    end_local_at: "2026-06-15T17:00:00",
    starts_at: "2026-06-15T12:00:00Z",
    ends_at: "2026-06-15T21:00:00Z",
    timezone: "America/New_York",
    dst_policy: "reject",
    tzdb_version: "2025b",
  },
  expect: "violated",
})

export const MaterializeWindow = Norn.decision({
  id: "time.window.materialize",
  title: "Materialize local working window to canonical UTC interval",
  version: "0.1.0",
  contract: "sub_decision",
  scopeRequired: ["orgId", "environmentId"],
  input: Schema.Struct({
    start_local_at: Schema.String,
    end_local_at: Schema.String,
    iana_timezone: Schema.String,
    dst_policy: Schema.Literals(["reject", "earlier", "later"]),
  }),
  output: Schema.Struct({
    starts_at: Schema.String,
    ends_at: Schema.String,
    duration_seconds: Schema.Finite,
    proper_interval: Schema.Boolean,
  }),
}, (input) => ({
  starts_at: Norn.resolveLocalTimestamp(input.start_local_at, input.iana_timezone, input.dst_policy),
  ends_at: Norn.resolveLocalTimestamp(input.end_local_at, input.iana_timezone, input.dst_policy),
  duration_seconds: Norn.secondsBetween(
    Norn.resolveLocalTimestamp(input.start_local_at, input.iana_timezone, input.dst_policy),
    Norn.resolveLocalTimestamp(input.end_local_at, input.iana_timezone, input.dst_policy),
  ),
  proper_interval: Norn.timestampBefore(
    Norn.resolveLocalTimestamp(input.start_local_at, input.iana_timezone, input.dst_policy),
    Norn.resolveLocalTimestamp(input.end_local_at, input.iana_timezone, input.dst_policy),
  ),
}))

export const MaterializeInterval = Norn.action({
  id: "time.window.materialize_interval",
  title: "Materialize working window into canonical UTC interval",
  version: "0.1.0",
  description: "Creates a time.MaterializedWindow bound to its linked Calendar and WorkingWindow, enforcing timezone, DST policy, tzdb 2025b pin, local wall bounds, and checked resolve_local_timestamp UTC resolution.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({
    id: Schema.String,
    calendar_id: Schema.String,
    working_window_id: Schema.String,
    calendar_date: Schema.String,
    start_local_at: Schema.String,
    end_local_at: Schema.String,
    starts_at: Schema.String,
    ends_at: Schema.String,
    timezone: Schema.String,
    dst_policy: Schema.Literals(["reject", "earlier", "later"]),
    tzdb_version: Schema.Literals(["2025b"]),
  }),
  output: Schema.Struct({
    id: Schema.String,
  }),
  slice: [
    { name: "calendar", type: Calendar, key: { property: { from: "input", property: "calendar_id" } } },
    { name: "working_window", type: WorkingWindow, key: { property: { from: "input", property: "working_window_id" } } },
    { name: "window_calendar", type: Calendar, key: { link: { from: "working_window", link: "calendar" } } },
  ],
  preconditions: [
    Norn.expression(
      Schema.Struct({
        slice: Schema.Struct({
          calendar: Schema.Struct({ id: Schema.String }),
          window_calendar: Schema.Array(Schema.Struct({ id: Schema.String })),
        }),
      }),
      (ctx) => ctx.slice.window_calendar.map((cal) => cal.id).includes(ctx.slice.calendar.id),
      { root: "context" },
    ),
    Norn.expression(
      Schema.Struct({
        input: Schema.Struct({ timezone: Schema.String, dst_policy: Schema.String, tzdb_version: Schema.String }),
        slice: Schema.Struct({
          calendar: Schema.Struct({ iana_timezone: Schema.String, dst_policy: Schema.String, tzdb_version: Schema.String }),
        }),
      }),
      (ctx) =>
        ctx.input.timezone === ctx.slice.calendar.iana_timezone &&
        ctx.input.dst_policy === ctx.slice.calendar.dst_policy &&
        ctx.input.tzdb_version === ctx.slice.calendar.tzdb_version,
      { root: "context" },
    ),
    Norn.expression(
      Schema.Struct({
        input: Schema.Struct({ calendar_date: Schema.String, start_local_at: Schema.String }),
        slice: Schema.Struct({
          working_window: Schema.Struct({ start_local: Schema.String }),
        }),
      }),
      (ctx) =>
        ctx.input.start_local_at ===
        Norn.take(ctx.input.calendar_date, 10) +
          "T" +
          (Norn.byteLength(ctx.slice.working_window.start_local) === 5
            ? Norn.take(ctx.slice.working_window.start_local, 5) + ":00"
            : Norn.take(ctx.slice.working_window.start_local, 8)),
      { root: "context" },
    ),
    Norn.expression(
      Schema.Struct({
        input: Schema.Struct({ calendar_date: Schema.String, end_local_at: Schema.String }),
        slice: Schema.Struct({
          working_window: Schema.Struct({ end_local: Schema.String }),
        }),
      }),
      (ctx) =>
        ctx.input.end_local_at ===
        Norn.take(ctx.input.calendar_date, 10) +
          "T" +
          (Norn.byteLength(ctx.slice.working_window.end_local) === 5
            ? Norn.take(ctx.slice.working_window.end_local, 5) + ":00"
            : Norn.take(ctx.slice.working_window.end_local, 8)),
      { root: "context" },
    ),
    Norn.expression(
      Schema.Struct({
        input: Schema.Struct({ calendar_date: Schema.String }),
        slice: Schema.Struct({
          working_window: Schema.Struct({
            weekday: Schema.Literals(["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"]),
          }),
        }),
      }),
      (ctx) =>
        Norn.daysBetween("2024-01-01", ctx.input.calendar_date) -
          Norn.floor(Norn.daysBetween("2024-01-01", ctx.input.calendar_date) / 7) * 7 ===
        (ctx.slice.working_window.weekday === "monday"
          ? 0
          : ctx.slice.working_window.weekday === "tuesday"
            ? 1
            : ctx.slice.working_window.weekday === "wednesday"
              ? 2
              : ctx.slice.working_window.weekday === "thursday"
                ? 3
                : ctx.slice.working_window.weekday === "friday"
                  ? 4
                  : ctx.slice.working_window.weekday === "saturday"
                    ? 5
                    : 6),
      { root: "context" },
    ),
    Norn.expression(
      Schema.Struct({
        input: Schema.Struct({ start_local_at: Schema.String, end_local_at: Schema.String, starts_at: Schema.String, ends_at: Schema.String }),
        slice: Schema.Struct({
          calendar: Schema.Struct({ iana_timezone: Schema.String, dst_policy: Schema.String }),
        }),
      }),
      (ctx) =>
        Norn.parseTimestamp(ctx.input.starts_at) ===
          Norn.resolveLocalTimestamp(
            ctx.input.start_local_at,
            ctx.slice.calendar.iana_timezone,
            ctx.slice.calendar.dst_policy,
          ) &&
        Norn.parseTimestamp(ctx.input.ends_at) ===
          Norn.resolveLocalTimestamp(
            ctx.input.end_local_at,
            ctx.slice.calendar.iana_timezone,
            ctx.slice.calendar.dst_policy,
          ) &&
        Norn.timestampBefore(ctx.input.starts_at, ctx.input.ends_at),
      { root: "context" },
    ),
  ],
  writes: [{ type: MaterializedWindow, change: "created" }],
  createLinks: [
    { link: "calendar", targetType: Calendar, targetIdField: "calendar_id" },
    { link: "working_window", targetType: WorkingWindow, targetIdField: "working_window_id" },
  ],
  effect: { kind: "upsert_input_object", type: MaterializedWindow, objectIdField: "id", writeMode: { kind: "create_only" } },
})
