import { Iso } from "@/components/iso"
import { directionOf, useLocale } from "@/lib/locale"
import { localizeMessage } from "@/lib/messages"

/**
 * A server message in the reader's language. The code is the contract and is
 * shown as sent, isolated; the text comes from the message catalog when the
 * code has one in the reader's language, else the server's own words.
 */
export function CodedMessage({ code, message, showCode = false, block = false }: { readonly code: string; readonly message: string; readonly showCode?: boolean; readonly block?: boolean }) {
  const { locale } = useLocale()
  const said = localizeMessage(code, message, locale)
  // The whole message (and its code) reads in the direction of the language it is said in.
  return (
    <span data-code={code} data-message-lang={said.lang} dir={directionOf(said.lang)} className={block ? "block" : undefined}>
      <span lang={said.lang} title={said.needsReview ? "Machine-proposed translation, awaiting review" : undefined}>{said.text}</span>
      {showCode && <> <Iso className="font-mono text-meta text-muted-foreground">{code}</Iso></>}
    </span>
  )
}
