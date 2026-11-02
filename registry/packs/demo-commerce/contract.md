# Synthetic commerce identities (D1b)

This extends the map-only ERPNext/Shopify packs with an independently authored
synthetic replay profile. Custom cross-reference fields are fixture extensions,
not standard webhook fields. One fictional organization uses fully scoped
references such as `demo/shipment/SYN-001` and `demo/catalog/GOLD`.

- `D1b-shipment-identity`: ERPNext Delivery Note and Shopify Fulfillment with
  the same explicit shipment reference produce the same object type and upsert
  key, with independent source/mapping provenance. Replays and arrival order
  preserve that identity. Neither mapping produces a stock effect.
- `D1b-catalog-not-holding`: ERPNext Item and Shopify ProductVariant map to
  catalog evidence, never a Holding. Shared SKUs do not merge distinct explicit
  catalog identities. No fuzzy matching, quantity or valuation is inferred.
- `D1b-unresolved-reference`: absent, empty or non-scalar references cannot
  produce a shipment candidate. Ambiguity must remain unresolved rather than
  choosing one reference. D3 must turn retained authenticated observations
  into findings; this cut does not persist findings or discard source evidence.

The mapping stage proves convergence to one shipment **target**, not durable
canonical promotion. `canonical_event` is a declared gap: D2/D3 and lane C must
bind qualified references, retain both source receipts, and admit at most one
economic event before claiming one depletion or counting accepted events.
Catalog-to-canonical Item binding, order lines, holding allocation, units,
valuation, authentication and authorization remain unqualified. These mappings
must not serve as a production ingress contract.
