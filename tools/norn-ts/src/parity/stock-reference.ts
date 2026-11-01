/** Executable reference for the receipt/delivery vertical, not posting authority.
 * Quantity and value are integer millionths. All arithmetic is exact. */
export type StockAmount = Readonly<{ quantity: bigint; value: bigint }>
export type ReferenceLayer = StockAmount & Readonly<{ receipt: string }>
export type ReferencePosition = Readonly<{ layers: readonly ReferenceLayer[] }>
export type ReferenceMovement =
  | Readonly<{ kind: "receipt"; id: string; quantity: bigint; value: bigint }>
  | Readonly<{ kind: "delivery"; id: string; quantity: bigint }>
export type ReferencePosting = Readonly<{
  before: ReferencePosition
  after: ReferencePosition
  stock: StockAmount
  counterpart: StockAmount
  allocations: readonly ReferenceLayer[]
}>

export function referenceBalance(position: ReferencePosition): StockAmount {
  return position.layers.reduce((sum, layer) => ({
    quantity: sum.quantity + layer.quantity, value: sum.value + layer.value,
  }), { quantity: 0n, value: 0n })
}

/** Half-up for nonnegative rational values; final depletion takes the residual. */
export function referenceShare(quantity: bigint, available: StockAmount): bigint {
  if (quantity < 0n || quantity > available.quantity || available.quantity <= 0n || available.value < 0n) {
    throw new Error("Invalid allocation")
  }
  if (quantity === available.quantity) return available.value
  return (2n * quantity * available.value + available.quantity) / (2n * available.quantity)
}

/** FIFO is expressed as intersections with cumulative receipt quantity intervals.
 * A production queue implementation can be compared against this formulation. */
export function referencePost(before: ReferencePosition, movement: ReferenceMovement): ReferencePosting {
  if (movement.quantity <= 0n) throw new Error("Quantity must be positive")
  if (before.layers.some(layer => layer.quantity <= 0n || layer.value < 0n)) throw new Error("Invalid layer")
  let after: ReferencePosition
  let allocations: readonly ReferenceLayer[]
  if (movement.kind === "receipt") {
    if (movement.value < 0n) throw new Error("Value must be nonnegative")
    if (before.layers.some(layer => layer.receipt === movement.id)) throw new Error("Duplicate receipt")
    after = { layers: [...before.layers, { receipt: movement.id, quantity: movement.quantity, value: movement.value }] }
    allocations = []
  } else {
    if (movement.quantity > referenceBalance(before).quantity) throw new Error("insufficient-stock")
    let start = 0n
    allocations = before.layers.map(layer => {
      const end = start + layer.quantity
      const quantity = movement.quantity <= start ? 0n : (movement.quantity < end ? movement.quantity : end) - start
      start = end
      return { receipt: layer.receipt, quantity, value: referenceShare(quantity, layer) }
    })
    after = { layers: before.layers.flatMap((layer, index) => {
      const taken = allocations[index]
      if (!taken) throw new Error("Missing reference allocation")
      return taken.quantity === layer.quantity ? [] : [{
        receipt: layer.receipt, quantity: layer.quantity - taken.quantity, value: layer.value - taken.value,
      }]
    }) }
  }
  const previous = referenceBalance(before)
  const current = referenceBalance(after)
  const stock = { quantity: current.quantity - previous.quantity, value: current.value - previous.value }
  return { before, after, stock, counterpart: { quantity: -stock.quantity, value: -stock.value }, allocations }
}

/** Algebraic reversal only: a caller must first establish no live dependents.
 * This does not implement cancellation admission or restore an arbitrary past. */
export function referenceInverse(posting: ReferencePosting): ReferencePosting {
  return {
    before: posting.after, after: posting.before,
    stock: posting.counterpart, counterpart: posting.stock, allocations: posting.allocations,
  }
}
