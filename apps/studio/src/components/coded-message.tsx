import { Iso } from "@/components/iso"
import { useLocale } from "@/lib/locale"
import { localizeMessage } from "@/lib/messages"

/**
 * A server message in the reader's language. The code is the contract and is
 * shown as sent, isolated; the text comes from the message catalog when the
 * code has one in the reader's language, else the server's own words.
 */
export function CodedMessage({ code, message, showCode = false }: { readonly code: string; readonly message: string; readonly showCode?: boolean }) {
  const { locale } = useLocale()
  const said = localizeMessage(code, message, locale)
  return (
    <span data-code={code} data-message-lang={said.lang}>
      <span lang={said.lang} dir={said.lang === "ar" ? "rtl" : "ltr"} title={said.needsReview ? "Machine-proposed translation, awaiting review" : undefined}>{said.text}</span>
      {showCode && <> <Iso className="font-mono text-meta text-muted-foreground">{code}</Iso></>}
    </span>
  )
}
