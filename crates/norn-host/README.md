# Native host perimeter

The ring-1 perimeter owns native host integration. The ring-0 `norn-api` crate
retains pure wire contracts and replay clock interfaces.

This first slice ports typed refusals and their HTTP response projection.
It exposes no routes, server, connector binding or analytical executor yet.
