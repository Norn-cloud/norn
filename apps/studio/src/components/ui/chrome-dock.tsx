import {
  createPopup,
  createSelection,
  definePrimitive,
  dispatch,
  openAttrs,
  popupContent,
  popupState,
  popupTrigger,
  type ChangeDetails,
  type PopupState,
} from "@ui-sdk/core"
import { popoverPreset } from "@ui-sdk/core/popover"
import { createParts } from "@ui-sdk/react/primitive"

type ChromeDockReason = "item-press" | "programmatic"

interface ChromeDockOptions {
  open?: boolean
  defaultOpen?: boolean
  onOpenChange?: (open: boolean, details: ChangeDetails<string>) => void
  value?: string
  defaultValue?: string
  onValueChange?: (value: string, details: ChangeDetails<ChromeDockReason>) => void
  items?: readonly string[]
  dismissible?: boolean
}

interface ChromeDockState extends PopupState {
  /** Internal single-item array managed by `createSelection`; kept separate from `value` so `onValueChange` fires once with a string. */
  selected: string[]
  items: readonly string[]
}

/**
 * First-party floating chrome & skin/dock tray primitive built on `@ui-sdk/core`.
 * Composes `createPopup` (`slot: "tray"`, Escape + outside dismissal, top-layer positioning) with
 * `createSelection` (single-select state for active skin/shell options) and exposes semantic
 * `data-state` attributes.
 */
export const chromeDockPrimitive = definePrimitive({
  name: "chrome-dock",
  options: {
    open: "boolean",
    defaultOpen: "boolean",
    onOpenChange: "callback",
    value: "string",
    defaultValue: "string",
    onValueChange: "callback",
    items: "value",
    dismissible: "boolean",
  },
  state: (options: ChromeDockOptions): ChromeDockState => ({
    ...popupState(options),
    selected: options.value !== undefined ? [options.value] : options.defaultValue !== undefined ? [options.defaultValue] : [],
    items: options.items ?? [],
  }),
  sync: (options: ChromeDockOptions) => {
    const patch: Partial<ChromeDockState> = {}
    if (options.open !== undefined) patch.open = options.open
    if (options.value !== undefined) patch.selected = [options.value]
    if (options.items !== undefined) patch.items = options.items
    return patch
  },
  api: (context) => {
    const popup = createPopup(context, { preset: popoverPreset, slot: "tray" })
    const selection = createSelection(context, {
      key: "selected",
      set: (next, details) => {
        const chosen = next[0]
        if (chosen === undefined) return false
        const current = context.store.get().selected[0]
        if (chosen === current) return false
        const canceled = dispatch(context, "onValueChange", chosen, details)
        if (canceled) return false
        if (context.options().value === undefined) {
          context.store.set({ selected: [chosen] })
        }
        return true
      },
      order: () => context.store.get().items,
      multiple: () => false,
    })
    return {
      ...popup,
      selection,
      select(key: string, event?: Event) {
        selection.choose(key, { reason: "item-press", event })
      },
      isSelected(key: string) {
        return selection.isSelected(key)
      },
    }
  },
  slots: (slot) => {
    const trigger = popupTrigger(slot)
    const content = popupContent(slot)
    return {
      root: slot({
        state: (state) => ({ open: state.open, value: state.selected[0] ?? "" }),
        attrs: (state) => ({
          "data-state": state.open ? "open" : "closed",
          ...openAttrs(state.open),
        }),
      }),
      trigger: slot({
        ...trigger,
        attrs: (state, part) => ({
          ...(trigger.attrs?.(state, part) ?? {}),
          "data-state": state.open ? "open" : "closed",
        }),
      }),
      tray: slot({
        ...content,
        attrs: (state, part) => ({
          ...(content.attrs?.(state, part) ?? {}),
          "data-state": state.open ? "open" : "closed",
        }),
      }),
      option: slot<{ value: string; disabled?: boolean }, { selected: boolean; disabled: boolean }>({
        props: {
          value: "string",
          disabled: "boolean",
        },
        state: (state, part) => ({
          selected: state.selected.includes(part.props.value),
          disabled: Boolean(part.props.disabled),
        }),
        attrs: (state, part) => {
          const selected = state.selected.includes(part.props.value)
          return {
            type: "button",
            "aria-pressed": String(selected),
            "data-state": selected ? "selected" : "idle",
            "data-selected": selected ? "" : undefined,
            "data-disabled": part.props.disabled ? "" : undefined,
            disabled: part.props.disabled ? "" : undefined,
          }
        },
        press: ({ api, props }, event) => {
          if (!props.disabled) api.select(props.value, event)
        },
      }),
    }
  },
  agent: { actions: ["setOpen"] },
})

const parts = createParts(chromeDockPrimitive, {
  root: "div",
  trigger: "button",
  tray: "div",
  option: "button",
})

export const ChromeDock = parts.Root
export const ChromeDockTrigger = parts.Trigger
export const ChromeDockTray = parts.Tray
export const ChromeDockOption = parts.Option
