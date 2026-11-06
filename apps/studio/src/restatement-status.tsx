/** The `#corrections` anchor while the corrections are read or failed to read, so deep links and the submenu land on it. */
export function CorrectionsStatus({ error }: { readonly error?: Error }) {
  return (
    <section id="corrections" className="scroll-mt-4" aria-live="polite">
      {error ? <p className="text-destructive">{error.message}</p> : <p className="text-muted-foreground">Reading corrections…</p>}
    </section>
  )
}
