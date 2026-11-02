# Native host conformance

HOST-CONFORMANCE-1 separates persisted Function/receipt and publication scenarios
from the pure analytics library. Only the declared development dependencies may
cross these boundaries. The frozen violation baseline and norn-metrics boundary
remain unchanged.

This cut declares an empty non-publishing Cargo test target. The adapter cuts
move the existing persisted-perform and authenticated CSV/PUBLISH scenarios
here. It adds no runtime library or publication authority.
