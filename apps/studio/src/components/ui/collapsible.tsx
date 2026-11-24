import * as React from "react"
import { Collapsible as CollapsiblePrimitive } from "@base-ui/react/collapsible"

function Collapsible({
  ...props
}: React.ComponentProps<typeof CollapsiblePrimitive.Root>) {
  return <CollapsiblePrimitive.Root data-slot="collapsible" {...props} />
}

function CollapsibleTrigger({
  asChild = false,
  render,
  children,
  ...props
}: Omit<React.ComponentProps<typeof CollapsiblePrimitive.Trigger>, "render"> & {
  asChild?: boolean
  render?: React.ComponentProps<typeof CollapsiblePrimitive.Trigger>["render"]
}) {
  const slotted = asChild && React.isValidElement(children)
  return (
    <CollapsiblePrimitive.Trigger
      data-slot="collapsible-trigger"
      render={render ?? (slotted ? (children as React.ReactElement<Record<string, unknown>>) : undefined)}
      {...props}
    >
      {slotted ? undefined : children}
    </CollapsiblePrimitive.Trigger>
  )
}

function CollapsibleContent({
  ...props
}: React.ComponentProps<typeof CollapsiblePrimitive.Panel>) {
  return (
    <CollapsiblePrimitive.Panel
      data-slot="collapsible-content"
      {...props}
    />
  )
}

export { Collapsible, CollapsibleTrigger, CollapsibleContent }
