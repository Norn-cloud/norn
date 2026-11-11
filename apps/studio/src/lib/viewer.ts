/**
 * Who is reading. It only names them: the server decides what they may see,
 * and its documents arrive already masked or refused. Sign-in fills the role.
 */
export interface Viewer {
  readonly name: string
  /** The Norn principal's role once signed in; `null` on the recorded demo. */
  readonly role: string | null
}

export const useViewer = (): Viewer => ({ name: "Recorded demo", role: null })
