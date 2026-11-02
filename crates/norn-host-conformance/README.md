# Native host conformance

HOST-CONFORMANCE-1 separates persisted Function/receipt and publication scenarios
from the pure analytics library. Only the declared development dependencies may
cross these boundaries. The frozen violation baseline and norn-metrics boundary
remain unchanged.

The adapter cut installs the non-publishing Cargo test targets here. This policy
cut declares their boundary before moving the existing scenarios; it adds no
runtime library or publication authority.
