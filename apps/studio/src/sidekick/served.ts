import { useQuery } from "@tanstack/react-query"

/** Whether this origin serves a Sidekick bridge: its config answers as JSON naming the socket. */
export async function sidekickServed(fetcher: typeof fetch = fetch, signal?: AbortSignal): Promise<boolean> {
  try {
    const response = await fetcher("/sidekick/config.json", {
      headers: { accept: "application/json" },
      cache: "no-store",
      ...signal === undefined ? {} : { signal },
    })
    if (!response.ok || !(response.headers.get("content-type") ?? "").includes("application/json")) return false
    const body = (await response.json()) as { sidekick?: unknown; socket?: unknown }
    return body.sidekick === 1 && body.socket === "/sidekick/ws"
  } catch {
    return false
  }
}

/** Whether this deployment serves Sidekick: asked once per page load. */
export function useSidekickServed(): boolean {
  return useQuery({
    queryKey: ["sidekick-served"],
    queryFn: () => sidekickServed(),
    staleTime: Number.POSITIVE_INFINITY,
    retry: false,
  }).data === true
}
