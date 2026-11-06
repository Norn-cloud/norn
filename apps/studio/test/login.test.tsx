import { expect, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { renderToStaticMarkup } from "react-dom/server"
import { LoginPage, SIGNED_UP, type Mode } from "../src/login.tsx"

const render = (initial: Mode) => renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><LoginPage initial={initial} /></QueryClientProvider>)

test("sign-in asks for email and password and offers the other ways in", () => {
  const html = render("sign-in")
  expect(html).toContain('autoComplete="current-password"')
  expect(html).toContain('type="email"')
  expect(html).toContain("Create account")
  expect(html).toContain("Email link")
  expect(html).not.toContain('name="name"')
})

test("creating an account asks for a name and a 12-character password", () => {
  const html = render("sign-up")
  expect(html).toContain('name="name"')
  expect(html).toContain('minLength="12"')
  expect(html).toContain('autoComplete="new-password"')
})

test("an email link needs only the address", () => {
  const html = render("email-link")
  expect(html).not.toContain('type="password"')
  expect(html).toContain("Send link")
})

test("after sign-up the page asks the user to sign in, never saying whether the address was new", () => {
  expect(SIGNED_UP).toBe("Account ready. Sign in with your email and password.")
  expect(SIGNED_UP).not.toMatch(/already|exists|registered|taken/i)
})
