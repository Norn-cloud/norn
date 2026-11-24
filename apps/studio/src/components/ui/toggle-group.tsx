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
  }
>({
  size: "default",
  variant: "default",
  spacing: 2,
  orientation: "horizontal",
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

  return (
    <ToggleGroupPrimitive
      data-slot="toggle-group"
      data-variant={variant}
      data-size={size}
      data-spacing={spacing}
      data-orientation={orientation}
      orientation={orientation}
      multiple={isSingle ? false : (multiple ?? true)}
      value={normalizedValue}
      defaultValue={normalizedDefaultValue}
      onValueChange={
        onValueChange
          ? (next) => {
              if (isSingle) {
                ;(onValueChange as (value: string) => void)(next[0] ?? "")
              } else {
                ;(onValueChange as (value: string[]) => void)(next)
              }
            }
          : undefined
      }
      style={{ "--gap": spacing } as React.CSSProperties}
      className={cn(
        "group/toggle-group flex w-fit flex-row items-center gap-[--spacing(var(--gap))] data-[spacing=0]:data-[variant=outline]:rounded-3xl data-vertical:flex-col data-vertical:items-stretch",
        className
      )}
      {...rest}
    >
      <ToggleGroupContext.Provider
        value={{ variant, size, spacing, orientation }}
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
  ...props
}: React.ComponentProps<typeof TogglePrimitive> &
  VariantProps<typeof toggleVariants>) {
  const context = React.useContext(ToggleGroupContext)

  return (
    <TogglePrimitive
      data-slot="toggle-group-item"
      data-variant={context.variant || variant}
      data-size={context.size || size}
      data-spacing={context.spacing}
      className={cn(
        "shrink-0 group-data-[spacing=0]/toggle-group:rounded-none group-data-[spacing=0]/toggle-group:px-3 group-data-[spacing=0]/toggle-group:shadow-none focus:z-10 focus-visible:z-10 group-data-[spacing=0]/toggle-group:has-data-[icon=inline-end]:pr-2.5 group-data-[spacing=0]/toggle-group:has-data-[icon=inline-start]:pl-2.5 group-data-horizontal/toggle-group:data-[spacing=0]:first:rounded-l-3xl group-data-vertical/toggle-group:data-[spacing=0]:first:rounded-t-3xl group-data-horizontal/toggle-group:data-[spacing=0]:last:rounded-r-3xl group-data-vertical/toggle-group:data-[spacing=0]:last:rounded-b-3xl data-pressed:bg-muted aria-pressed:bg-muted group-data-horizontal/toggle-group:data-[spacing=0]:data-[variant=outline]:border-l-0 group-data-vertical/toggle-group:data-[spacing=0]:data-[variant=outline]:border-t-0 group-data-horizontal/toggle-group:data-[spacing=0]:data-[variant=outline]:first:border-l group-data-vertical/toggle-group:data-[spacing=0]:data-[variant=outline]:first:border-t",
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
