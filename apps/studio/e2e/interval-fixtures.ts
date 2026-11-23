/**
 * Two unrelated synthetic worlds whose records are time intervals, for the Timeline view and the
 * form's pickers: equipment bookings (`facilities.EquipmentBooking` implements `core.TimeInterval`
 * directly) and room reservations (`venues.RoomReservation` implements `venues.Reservation`, which
 * extends it). Shared by the unit tests and the Playwright stand-in, so both read one document.
 *
 * `core.TimeInterval` follows the contract #1584 sets (`starts_at`, `ends_at`, optional IANA
 * `timezone`); it is declared here only until the registry ships it. The action schemas carry
 * JSON Schema `format` on their date and time inputs, the shape requested of the backend lane:
 * the served input dialect does not admit `format` yet (see the PR's follow-ups).
 */

const entity = (name: string, body: Record<string, unknown>, kind: "objectType" | "interface" = "objectType") => ({
  artifactId: `${name}.${kind === "objectType" ? "entity" : "interface"}`, dependencies: [], irVersion: "0.1", kind: "entity", lifecycle: "active",
  name: name.slice(name.lastIndexOf(".") + 1), namespace: name.slice(0, name.lastIndexOf(".")),
  payload: { entityVersion: "2.0", [kind]: { name, ...body } }, provenance: { source: "system" }, scope: {}, version: "0.1.0",
})

const instant = { kind: "timestamp", timezone: "offset" }
const text = { kind: "text" }
const one = (id: string, target: string, required = true) => ({ id, target, cardinality: { from: "many", to: "one", ...required ? { required: true } : {} } })

export const INTERVAL_ARTIFACTS = [
  entity("core.TimeInterval", {
    role: "happening",
    description: "Something that holds a proper span of time: it starts before it ends.",
    properties: [{ id: "starts_at", type: instant }, { id: "ends_at", type: instant }, { id: "timezone", type: text, constraints: { nullable: true } }],
  }, "interface"),
  entity("facilities.Equipment", { properties: [{ id: "label", type: text }] }),
  entity("facilities.Team", { properties: [{ id: "label", type: text }] }),
  entity("facilities.EquipmentBooking", {
    implements: ["core.TimeInterval"],
    properties: [{ id: "label", type: text }, { id: "starts_at", type: instant }, { id: "ends_at", type: instant }, { id: "timezone", type: text, constraints: { nullable: true } }, { id: "purpose", type: text, constraints: { nullable: true } }],
    links: [one("equipment", "facilities.Equipment"), one("booked_by", "facilities.Team", false), { id: "accessories", target: "facilities.Equipment", cardinality: { from: "many", to: "many" } }],
  }),
  entity("venues.Room", { properties: [{ id: "label", type: text }] }),
  entity("venues.Reservation", { extends: ["core.TimeInterval"], properties: [] }, "interface"),
  entity("venues.RoomReservation", {
    implements: ["venues.Reservation"],
    properties: [{ id: "label", type: text }, { id: "starts_at", type: instant }, { id: "ends_at", type: instant }, { id: "timezone", type: text, constraints: { nullable: true } }, { id: "setup_on", type: { kind: "date" }, constraints: { nullable: true } }],
    links: [one("room", "venues.Room")],
  }),
] as const

/** One stored record: its type, id, values and its to-one link targets by link id. */
export interface IntervalRecord { readonly objectType: string; readonly id: string; values: Record<string, string | null>; links: Record<string, string>; version: number }

/** The week of Monday 23 November 2026, which the browser rows fix as "now". */
export const NOW = "2026-11-23T10:00:00Z"

export function intervalRecords(): IntervalRecord[] {
  const equipment = (n: number, label: string) => ({ objectType: "facilities.Equipment", id: `equipment/${n}`, values: { label }, links: {}, version: 1 })
  const room = (n: number, label: string) => ({ objectType: "venues.Room", id: `room/${n}`, values: { label }, links: {}, version: 1 })
  const booking = (n: number, label: string, starts: string, ends: string, on: number, zone: string | null = null) =>
    ({ objectType: "facilities.EquipmentBooking", id: `booking/${n}`, values: { label, starts_at: starts, ends_at: ends, timezone: zone, purpose: null }, links: { equipment: `equipment/${on}` }, version: 1 })
  const reservation = (n: number, label: string, starts: string, ends: string, inRoom: number, zone: string | null = null) =>
    ({ objectType: "venues.RoomReservation", id: `reservation/${n}`, values: { label, starts_at: starts, ends_at: ends, timezone: zone, setup_on: null }, links: { room: `room/${inRoom}` }, version: 1 })
  return [
    equipment(1, "Laser cutter"), equipment(2, "Forklift"), equipment(3, "Thermal camera"),
    ...Array.from({ length: 30 }, (_, i) => equipment(10 + i, `Spare unit ${String(i + 1).padStart(2, "0")}`)),
    room(1, "Board room"), room(2, "Studio B"),
    booking(1, "Prototype run", "2026-11-23T08:00:00Z", "2026-11-23T12:00:00Z", 1),
    // Shares the morning with booking 1 on the same cutter: the lane stacks them and marks both.
    booking(2, "Rush order", "2026-11-23T11:00:00Z", "2026-11-23T14:00:00Z", 1),
    booking(3, "Warehouse move", "2026-11-24T06:00:00+09:00", "2026-11-24T15:00:00+09:00", 2, "Asia/Tokyo"),
    // Starts in the week before: the bar is clipped at the window's start.
    booking(4, "Inspection", "2026-11-20T09:00:00Z", "2026-11-25T17:00:00Z", 3),
    booking(5, "December service", "2026-12-14T09:00:00Z", "2026-12-14T11:00:00Z", 2),
    reservation(1, "Quarterly review", "2026-11-25T13:00:00Z", "2026-11-25T15:30:00Z", 1),
    reservation(2, "Podcast recording", "2026-11-26T09:00:00-05:00", "2026-11-26T11:00:00-05:00", 2, "America/New_York"),
  ]
}

const NONE = { irreversible: false, destructive: false, money: [] as string[] }

/** Upsert a booking: an instant picker each end, a record picker for the equipment, a date and a time of day. */
export const BOOK_EQUIPMENT = {
  actionId: "facilities.booking.book",
  title: "Book equipment",
  description: "Hold a piece of equipment for a span of time.",
  objectKind: "facilities.EquipmentBooking",
  effect: "upsert_input_object",
  objectIdField: "id",
  uiEffects: NONE,
  inputLinks: [{ field: "equipment", objectKind: "facilities.Equipment" }],
  inputSchema: {
    type: "object", additionalProperties: false, required: ["id", "label", "equipment", "starts_at", "ends_at"],
    properties: {
      id: { type: "string", title: "Booking id" },
      label: { type: "string", title: "Title" },
      equipment: { type: "string", title: "Equipment" },
      starts_at: { type: "string", format: "date-time", title: "Starts" },
      ends_at: { type: "string", format: "date-time", title: "Ends" },
      inspect_on: { type: ["string", "null"], format: "date", title: "Inspect on" },
      daily_check: { type: ["string", "null"], format: "time", title: "Daily check at" },
    },
  },
} as const

/** Reserve a room: one date picker, one record picker. */
export const RESERVE_ROOM = {
  actionId: "venues.reservation.reserve",
  title: "Reserve room",
  description: null,
  objectKind: "venues.RoomReservation",
  effect: "upsert_input_object",
  objectIdField: "id",
  uiEffects: NONE,
  inputLinks: [{ field: "room", objectKind: "venues.Room" }],
  inputSchema: {
    type: "object", additionalProperties: false, required: ["id", "label", "room", "starts_at", "ends_at"],
    properties: {
      id: { type: "string", title: "Reservation id" },
      label: { type: "string", title: "Title" },
      room: { type: "string", title: "Room" },
      starts_at: { type: "string", format: "date-time", title: "Starts" },
      ends_at: { type: "string", format: "date-time", title: "Ends" },
      setup_on: { type: ["string", "null"], format: "date", title: "Set up on" },
    },
  },
} as const

/** A transition the cell offers on a booking: the drawer lists it from the cell's state-narrowed catalog. */
export const CANCEL_BOOKING = {
  actionId: "facilities.booking.cancel",
  title: "Cancel booking",
  description: null,
  objectKind: "facilities.EquipmentBooking",
  effect: "fire_transition",
  objectIdField: "id",
  toState: "cancelled",
  uiEffects: NONE,
  inputSchema: { type: "object", additionalProperties: false, required: ["id"], properties: { id: { type: "string", title: "Booking id" } } },
} as const

export const INTERVAL_ACTIONS = [BOOK_EQUIPMENT, CANCEL_BOOKING, RESERVE_ROOM] as const
