import { Context, Effect, Layer } from "effect"
import { Norn } from "@norn/authoring"

class Mailer extends Context.Service<Mailer, { readonly send: (to: string) => Effect.Effect<void> }>()("Mailer") {}

export const MailerLive = Layer.succeed(Mailer, { send: () => Effect.void })

export default Norn.flow(
  { id: "layer-context", version: "1" },
  Effect.gen(function* () {
    const mailer = yield* Effect.service(Mailer)
    return { sent: mailer !== undefined }
  }).pipe(Effect.provide(MailerLive)),
)
