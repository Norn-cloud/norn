import { mock } from "bun:test"

// `cloudflare:workers` exists only in the Workers runtime; the Durable Object base class is all the Worker imports from it.
mock.module("cloudflare:workers", () => ({ DurableObject: class {} }))
