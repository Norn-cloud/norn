import * as React from "react"
import { cn } from "@/lib/utils"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import {
  InputGroup,
  InputGroupAddon,
} from "@/components/ui/input-group"
import { SearchIcon, CheckIcon } from "lucide-react"

interface RegisteredItem {
  readonly id: string
  readonly value: string
  readonly disabled: boolean
  readonly onSelect?: (value: string) => void
  readonly element: () => HTMLDivElement | null
}

interface CommandContextValue {
  readonly listId: string
  readonly search: string
  readonly setSearch: (next: string) => void
  readonly shouldFilter: boolean
  readonly filter: (value: string, search: string, keywords?: string[]) => number
  readonly activeValue: string | null
  readonly setActiveValue: (value: string | null) => void
  readonly registerItem: (item: RegisteredItem) => () => void
  readonly move: (delta: number | "first" | "last") => void
  readonly selectActive: () => void
  readonly visibleCount: number
  readonly itemIdOf: (value: string) => string | undefined
}

const defaultFilter = (value: string, search: string, keywords?: string[]): number => {
  const query = search.trim().toLowerCase()
  if (query === "") return 1
  const haystack = [value, ...(keywords ?? [])].join(" ").toLowerCase()
  return haystack.includes(query) ? 1 : 0
}

const CommandContext = React.createContext<CommandContextValue | null>(null)

function useCommandContext() {
  const ctx = React.useContext(CommandContext)
  if (!ctx) throw new Error("Command parts must be rendered inside <Command>")
  return ctx
}

type CommandProps = Omit<React.ComponentProps<"div">, "onChange"> & {
  label?: string
  shouldFilter?: boolean
  filter?: (value: string, search: string, keywords?: string[]) => number
  value?: string
  defaultValue?: string
  onValueChange?: (value: string) => void
  loop?: boolean
}

function Command({
  className,
  label,
  shouldFilter = true,
  filter = defaultFilter,
  value: controlledValue,
  defaultValue,
  onValueChange,
  loop = true,
  onKeyDown,
  children,
  ...props
}: CommandProps) {
  const listId = React.useId()
  const [search, setSearch] = React.useState("")
  const [uncontrolledActive, setUncontrolledActive] = React.useState<string | null>(defaultValue ?? null)
  const itemsRef = React.useRef<RegisteredItem[]>([])
  const [visibleCount, setVisibleCount] = React.useState(0)

  const activeValue = controlledValue ?? uncontrolledActive
  const setActiveValue = React.useCallback(
    (next: string | null) => {
      if (controlledValue === undefined) setUncontrolledActive(next)
      if (next !== null) onValueChange?.(next)
    },
    [controlledValue, onValueChange],
  )

  const syncVisible = React.useCallback(() => {
    const items = itemsRef.current
    setVisibleCount(items.length)
    setUncontrolledActive((current) => {
      if (controlledValue !== undefined) return current
      const enabled = items.filter((item) => !item.disabled)
      if (enabled.length === 0) return null
      if (current !== null && enabled.some((item) => item.value === current)) return current
      return enabled[0]!.value
    })
  }, [controlledValue])

  const registerItem = React.useCallback(
    (item: RegisteredItem) => {
      const existingIndex = itemsRef.current.findIndex((entry) => entry.id === item.id)
      if (existingIndex >= 0) {
        itemsRef.current[existingIndex] = item
      } else {
        itemsRef.current = [...itemsRef.current, item]
      }
      syncVisible()
      return () => {
        itemsRef.current = itemsRef.current.filter((entry) => entry.id !== item.id)
        syncVisible()
      }
    },
    [syncVisible],
  )

  const move = React.useCallback(
    (delta: number | "first" | "last") => {
      const enabled = itemsRef.current.filter((item) => !item.disabled)
      if (enabled.length === 0) {
        setActiveValue(null)
        return
      }
      let nextIndex: number
      if (delta === "first") {
        nextIndex = 0
      } else if (delta === "last") {
        nextIndex = enabled.length - 1
      } else {
        const currentIndex = enabled.findIndex((item) => item.value === activeValue)
        if (currentIndex === -1) {
          nextIndex = delta > 0 ? 0 : enabled.length - 1
        } else if (loop) {
          nextIndex = (currentIndex + delta + enabled.length) % enabled.length
        } else {
          nextIndex = Math.max(0, Math.min(enabled.length - 1, currentIndex + delta))
        }
      }
      const target = enabled[nextIndex]
      if (!target) return
      setActiveValue(target.value)
      target.element()?.scrollIntoView({ block: "nearest" })
    },
    [activeValue, loop, setActiveValue],
  )

  const selectActive = React.useCallback(() => {
    const active = itemsRef.current.find((item) => !item.disabled && item.value === activeValue)
      ?? itemsRef.current.find((item) => !item.disabled)
    if (active) active.onSelect?.(active.value)
  }, [activeValue])

  const itemIdOf = React.useCallback(
    (value: string) => itemsRef.current.find((item) => item.value === value)?.id,
    [],
  )

  const handleKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    onKeyDown?.(event)
    if (event.defaultPrevented) return
    if (event.key === "ArrowDown") {
      event.preventDefault()
      move(1)
    } else if (event.key === "ArrowUp") {
      event.preventDefault()
      move(-1)
    } else if (event.key === "Home") {
      event.preventDefault()
      move("first")
    } else if (event.key === "End") {
      event.preventDefault()
      move("last")
    } else if (event.key === "Enter") {
      event.preventDefault()
      selectActive()
    }
  }

  const contextValue = React.useMemo<CommandContextValue>(
    () => ({
      listId,
      search,
      setSearch,
      shouldFilter,
      filter,
      activeValue,
      setActiveValue,
      registerItem,
      move,
      selectActive,
      visibleCount,
      itemIdOf,
    }),
    [listId, search, shouldFilter, filter, activeValue, setActiveValue, registerItem, move, selectActive, visibleCount, itemIdOf],
  )

  return (
    <CommandContext.Provider value={contextValue}>
      <div
        data-slot="command"
        aria-label={label}
        onKeyDown={handleKeyDown}
        className={cn(
          "flex size-full flex-col overflow-hidden rounded-4xl bg-popover p-1 text-popover-foreground",
          className
        )}
        {...props}
      >
        {children}
      </div>
    </CommandContext.Provider>
  )
}

function CommandDialog({
  title = "Command Palette",
  description = "Search for a command to run...",
  children,
  className,
  showCloseButton = false,
  ...props
}: React.ComponentProps<typeof Dialog> & {
  title?: string
  description?: string
  className?: string
  showCloseButton?: boolean
}) {
  return (
    <Dialog {...props}>
      <DialogHeader className="sr-only">
        <DialogTitle>{title}</DialogTitle>
        <DialogDescription>{description}</DialogDescription>
      </DialogHeader>
      <DialogContent
        className={cn(
          "top-1/3 translate-y-0 overflow-hidden rounded-4xl! p-0",
          className
        )}
        showCloseButton={showCloseButton}
      >
        {children}
      </DialogContent>
    </Dialog>
  )
}

function CommandInput({
  className,
  value,
  onValueChange,
  onChange,
  ...props
}: Omit<React.ComponentProps<"input">, "value"> & {
  value?: string
  onValueChange?: (value: string) => void
}) {
  const ctx = useCommandContext()
  const currentValue = value ?? ctx.search
  const activeDescendant = ctx.activeValue !== null ? ctx.itemIdOf(ctx.activeValue) : undefined

  return (
    <div data-slot="command-input-wrapper" className="p-1 pb-0">
      <InputGroup className="h-9 bg-input/50">
        <input
          type="text"
          role="combobox"
          aria-expanded="true"
          aria-autocomplete="list"
          aria-controls={ctx.listId}
          aria-activedescendant={activeDescendant}
          data-slot="command-input"
          value={currentValue}
          onChange={(event) => {
            const next = event.target.value
            ctx.setSearch(next)
            onValueChange?.(next)
            onChange?.(event)
          }}
          className={cn(
            "w-full bg-transparent px-2 text-sm outline-hidden disabled:cursor-not-allowed disabled:opacity-50",
            className
          )}
          {...props}
        />
        <InputGroupAddon>
          <SearchIcon className="size-4 shrink-0 opacity-50" />
        </InputGroupAddon>
      </InputGroup>
    </div>
  )
}

function CommandList({
  className,
  ...props
}: React.ComponentProps<"div">) {
  const ctx = useCommandContext()
  return (
    <div
      id={ctx.listId}
      role="listbox"
      data-slot="command-list"
      className={cn(
        "no-scrollbar max-h-72 scroll-py-1 overflow-x-hidden overflow-y-auto outline-none",
        className
      )}
      {...props}
    />
  )
}

function CommandEmpty({
  className,
  ...props
}: React.ComponentProps<"div">) {
  const ctx = useCommandContext()
  if (ctx.shouldFilter && ctx.visibleCount > 0) return null
  return (
    <div
      role="presentation"
      data-slot="command-empty"
      className={cn("py-6 text-center text-sm", className)}
      {...props}
    />
  )
}

function CommandGroup({
  className,
  heading,
  children,
  ...props
}: React.ComponentProps<"div"> & {
  heading?: React.ReactNode
}) {
  const headingId = React.useId()
  return (
    <div
      role="group"
      aria-labelledby={heading !== undefined ? headingId : undefined}
      data-slot="command-group"
      className={cn(
        "overflow-hidden p-1.5 text-foreground",
        className
      )}
      {...props}
    >
      {heading !== undefined && (
        <div id={headingId} data-slot="command-group-heading" className="px-3 py-2 text-xs font-medium text-muted-foreground">
          {heading}
        </div>
      )}
      {children}
    </div>
  )
}

function CommandSeparator({
  className,
  ...props
}: React.ComponentProps<"div">) {
  return (
    <div
      role="separator"
      data-slot="command-separator"
      className={cn("my-1.5 h-px bg-border/50", className)}
      {...props}
    />
  )
}

function CommandItem({
  className,
  children,
  value,
  keywords,
  disabled = false,
  onSelect,
  onClick,
  onPointerMove,
  ...props
}: Omit<React.ComponentProps<"div">, "onSelect"> & {
  value?: string
  keywords?: string[]
  disabled?: boolean
  onSelect?: (value: string) => void
}) {
  const ctx = useCommandContext()
  const itemId = React.useId()
  const ref = React.useRef<HTMLDivElement>(null)
  const resolvedValue = value ?? (typeof children === "string" ? children : itemId)
  const matches = !ctx.shouldFilter || ctx.filter(resolvedValue, ctx.search, keywords) > 0
  const onSelectRef = React.useRef(onSelect)
  onSelectRef.current = onSelect

  React.useEffect(() => {
    if (!matches) return
    return ctx.registerItem({
      id: itemId,
      value: resolvedValue,
      disabled,
      onSelect: (val) => onSelectRef.current?.(val),
      element: () => ref.current,
    })
  }, [ctx.registerItem, itemId, resolvedValue, disabled, matches])

  if (!matches) return null

  const selected = !disabled && ctx.activeValue === resolvedValue

  return (
    <div
      ref={ref}
      id={itemId}
      role="option"
      aria-selected={selected}
      aria-disabled={disabled || undefined}
      data-slot="command-item"
      data-value={resolvedValue}
      data-selected={selected ? "true" : "false"}
      data-disabled={disabled ? "true" : undefined}
      onPointerMove={(event) => {
        if (!disabled && ctx.activeValue !== resolvedValue) ctx.setActiveValue(resolvedValue)
        onPointerMove?.(event)
      }}
      onClick={(event) => {
        if (!disabled) {
          ctx.setActiveValue(resolvedValue)
          onSelect?.(resolvedValue)
        }
        onClick?.(event)
      }}
      className={cn(
        "group/command-item relative flex cursor-default items-center gap-2 rounded-2xl px-3 py-2 text-sm font-medium outline-hidden select-none in-data-[slot=dialog-content]:rounded-3xl data-[disabled=true]:pointer-events-none data-[disabled=true]:opacity-50 data-selected:bg-muted data-selected:text-foreground [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4 data-selected:*:[svg]:text-foreground",
        className
      )}
      {...props}
    >
      {children}
      <CheckIcon className="ml-auto opacity-0 group-has-data-[slot=command-shortcut]/command-item:hidden group-data-[checked=true]/command-item:opacity-100" />
    </div>
  )
}

function CommandShortcut({
  className,
  ...props
}: React.ComponentProps<"span">) {
  return (
    <span
      data-slot="command-shortcut"
      dir="ltr"
      className={cn(
        "ms-auto text-xs tracking-widest [unicode-bidi:isolate] text-muted-foreground group-data-selected/command-item:text-foreground",
        className
      )}
      {...props}
    />
  )
}

export {
  Command,
  CommandDialog,
  CommandInput,
  CommandList,
  CommandEmpty,
  CommandGroup,
  CommandItem,
  CommandShortcut,
  CommandSeparator,
}
