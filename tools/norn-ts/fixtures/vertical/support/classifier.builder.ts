// The builder twin of classifier.source.ts.
import { S, fn, taggedError } from "../../../src/builder/index.ts"

export const UnsupportedLanguage = taggedError("UnsupportedLanguage", S.record({ locale: S.text() }))

export const classifyIntent = fn({
  id: "norn.classify.whatsapp-intent",
  version: "1",
  input: S.record({ channel: S.text(), locale: S.text(), messageText: S.text() }),
  output: S.record({ intent: S.text(), confidence: S.number, labels: S.array(S.text()) }),
  errors: [UnsupportedLanguage],
})
