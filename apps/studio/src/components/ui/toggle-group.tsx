"use client"

import * as React from "react"
import { Toggle as TogglePrimitive } from "@base-ui/react/toggle"
import { ToggleGroup as ToggleGroupPrimitive } from "@base-ui/react/toggle-group"
import { type VariantProps } from "class-variance-authority"
import { cn } from "@/lib/utils"
import { toggleVariants } from "@/components/ui/toggle"

const ToggleGroupContext = React.createContext<
  VariantProps<typeof toggleVariants> & {
    spacing?: number
    orientation?: "horizontal" | "vertical"
    single?: boolean
    selected?: readonly string[]
    consumeArrowSelect?: () => boolean
    selectRadio?: (value: string) => void
  }
>({
  size: "default",
  variant: "default",
  spacing: 2,
  orientation: "horizontal",
  single: false,
  selected: [],
})

type ToggleGroupBaseProps = Omit<
  React.ComponentProps<typeof ToggleGroupPrimitive>,
  "value" | "defaultValue" | "onValueChange" | "multiple"
> &
  VariantProps<typeof toggleVariants> & {
    spacing?: number
    orientation?: "horizontal" | "vertical"
  }

type SingleToggleGroupProps = ToggleGroupBaseProps & {
  type: "single"
  value?: string
  defaultValue?: string
  onValueChange?: (value: string) => void
}

type MultipleToggleGroupProps = ToggleGroupBaseProps & {
  type?: "multiple"
  multiple?: boolean
  value?: readonly string[]
  defaultValue?: readonly string[]
  onValueChange?: (value: string[]) => void
}

type ToggleGroupProps = SingleToggleGroupProps | MultipleToggleGroupProps

function ToggleGroup(props: ToggleGroupProps) {
  const {
    className,
    variant,
    size,
    spacing = 2,
    orientation = "horizontal",
    children,
  } = props

  const isSingle = props.type === "single"
  const {
    type: _type,
    value,
    defaultValue,
    onValueChange,
    multiple,
    role,
    className: _c,
    variant: _v,
    size: _s,
    spacing: _sp,
    orientation: _o,
    children: _ch,
    ...rest
  } = props as ToggleGroupBaseProps & {
    type?: "single" | "multiple"
    multiple?: boolean
    value?: string | readonly string[]
    defaultValue?: string | readonly string[]
    onValueChange?: ((value: string) => void) | ((value: string[]) => void)
  }

  const normalizedValue =
    value === undefined
      ? undefined
      : Array.isArray(value)
        ? value
        : value === ""
          ? []
          : [value]

  const normalizedDefaultValue =
    defaultValue === undefined
      ? undefined
      : Array.isArray(defaultValue)
        ? defaultValue
        : defaultValue === ""
          ? []
          : [defaultValue]

  const [uncontrolled, setUncontrolled] = React.useState<readonly string[]>(
    () => normalizedDefaultValue ?? []
  )
  const selected = normalizedValue ?? uncontrolled
  const pendingArrowSelect = React.useRef(false)
  const { onKeyDownCapture, ...groupRest } = rest

  const consumeArrowSelect = React.useCallback(() => {
    const active = pendingArrowSelect.current
    pendingArrowSelect.current = false
    return active
  }, [])

  const selectRadio = React.useCallback(
    (nextValue: string) => {
      if (!isSingle) return
      if (selected[0] === nextValue) return
      if (normalizedValue === undefined) setUncontrolled([nextValue])
      ;(onValueChange as ((value: string) => void) | undefined)?.(nextValue)
    },
    [isSingle, normalizedValue, onValueChange, selected],
  )

  return (
    <ToggleGroupPrimitive
      data-slot="toggle-group"
      data-variant={variant}
      data-size={size}
      data-spacing={spacing}
      data-orientation={orientation}
      role={role ?? (isSingle ? "radiogroup" : "group")}
      orientation={orientation}
      multiple={isSingle ? false : (multiple ?? true)}
      value={selected}
      onKeyDownCapture={(event) => {
        const isOrientationArrow =
          orientation === "vertical"
            ? event.key === "ArrowUp" || event.key === "ArrowDown"
            : event.key === "ArrowLeft" || event.key === "ArrowRight"
        if (isSingle && isOrientationArrow) {
          pendingArrowSelect.current = true
          // Base UI moves roving focus synchronously during keydown; clear immediately
          // after so a no-op arrow press can never arm a later pointer or Tab focus.
          queueMicrotask(() => {
            pendingArrowSelect.current = false
          })
        }
        onKeyDownCapture?.(event)
      }}
      onValueChange={(next) => {
        if (normalizedValue === undefined) setUncontrolled(next)
        if (!onValueChange) return
        if (isSingle) {
          ;(onValueChange as (value: string) => void)(next[0] ?? "")
        } else {
          ;(onValueChange as (value: string[]) => void)(next)
        }
      }}
      style={{ "--gap": spacing } as React.CSSProperties}
      className={cn(
        "group/toggle-group flex w-fit flex-row items-center gap-[--spacing(var(--gap))] data-[spacing=0]:data-[variant=outline]:rounded-3xl data-vertical:flex-col data-vertical:items-stretch",
        className
      )}
      {...groupRest}
    >
      <ToggleGroupContext.Provider
        value={{
          variant,
          size,
          spacing,
          orientation,
          single: isSingle,
          selected,
          consumeArrowSelect,
          selectRadio,
        }}
      >
        {children}
      </ToggleGroupContext.Provider>
    </ToggleGroupPrimitive>
  )
}

function ToggleGroupItem({
  className,
  children,
  variant = "default",
  size = "default",
  value,
  onFocus,
  ...props
}: React.ComponentProps<typeof TogglePrimitive> &
  VariantProps<typeof toggleVariants>) {
  const context = React.useContext(ToggleGroupContext)
  const isRadio = Boolean(context.single)
  const checked = value !== undefined && (context.selected?.includes(value) ?? false)

  return (
    <TogglePrimitive
      data-slot="toggle-group-item"
      data-variant={context.variant || variant}
      data-size={context.size || size}
      data-spacing={context.spacing}
      value={value}
      onFocus={(event) => {
        if (isRadio && value !== undefined && context.consumeArrowSelect?.()) {
          context.selectRadio?.(value)
        }
        onFocus?.(event)
      }}
      render={
        isRadio
          ? (toggleProps) => {
              const { "aria-pressed": _omitted, ...radioProps } = toggleProps as Record<string, unknown>
              return <button {...radioProps} role="radio" aria-checked={checked} />
            }
          : undefined
      }
      className={cn(
        "shrink-0 group-data-[spacing=0]/toggle-group:rounded-none group-data-[spacing=0]/toggle-group:px-3 group-data-[spacing=0]/toggle-group:shadow-none focus:z-10 focus-visible:z-10 group-data-[spacing=0]/toggle-group:has-data-[icon=inline-end]:pr-2.5 group-data-[spacing=0]/toggle-group:has-data-[icon=inline-start]:pl-2.5 group-data-horizontal/toggle-group:data-[spacing=0]:first:rounded-l-3xl group-data-vertical/toggle-group:data-[spacing=0]:first:rounded-t-3xl group-data-horizontal/toggle-group:data-[spacing=0]:last:rounded-r-3xl group-data-vertical/toggle-group:data-[spacing=0]:last:rounded-b-3xl data-pressed:bg-muted aria-pressed:bg-muted aria-checked:bg-muted group-data-horizontal/toggle-group:data-[spacing=0]:data-[variant=outline]:border-l-0 group-data-vertical/toggle-group:data-[spacing=0]:data-[variant=outline]:border-t-0 group-data-horizontal/toggle-group:data-[spacing=0]:data-[variant=outline]:first:border-l group-data-vertical/toggle-group:data-[spacing=0]:data-[variant=outline]:first:border-t",
        toggleVariants({
          variant: context.variant || variant,
          size: context.size || size,
        }),
        className
      )}
      {...props}
    >
      {children}
    </TogglePrimitive>
  )
}

export { ToggleGroup, ToggleGroupItem }
