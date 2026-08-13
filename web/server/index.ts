/**
 * The Asaborake web server.
 *
 * Elysia sits in front of the Rust engine and is the only thing that faces the
 * network. The engine binds loopback and has no authentication of its own, so
 * putting a server in front of it is what makes exposing the UI safe at all —
 * and it is where session handling and any future EPGStation proxying belong.
 *
 * In production it also serves the built client, so a deployment is one
 * process and one port rather than two.
 *
 * Routing is a single explicit dispatch rather than a set of registered
 * patterns. There are only three cases, and two of them are wildcards that a
 * matcher has to be told how to order; spelling the order out is shorter than
 * the rules would be, and cannot be got wrong by a later edit.
 */

import { Elysia } from "elysia";

/** Where the Rust engine is listening. */
const ENGINE = process.env.ASABORAKE_ENGINE ?? "http://127.0.0.1:8081";

/** Port this server listens on. */
const PORT = Number(process.env.PORT ?? 3001);

/** Whether to serve the built client from disk. */
const SERVE_CLIENT = process.env.ASABORAKE_SERVE_CLIENT !== "false";

/** Where the built client lives. */
const CLIENT_ROOT = new URL("../dist/", import.meta.url).pathname;

/**
 * Forward a request to the engine, preserving method, body and query.
 *
 * Deliberately a passthrough rather than a re-implementation: the engine owns
 * the shapes, and a second definition of them here would drift.
 */
async function forward(request: Request, pathname: string): Promise<Response> {
  const incoming = new URL(request.url);
  const target = new URL(pathname + incoming.search, ENGINE);

  const init: RequestInit = {
    method: request.method,
    headers: {
      // Only content negotiation is carried through; hop-by-hop headers and
      // anything the browser set about the outer origin are not the engine's
      // business.
      "content-type": request.headers.get("content-type") ?? "application/json",
      accept: request.headers.get("accept") ?? "application/json",
    },
  };
  if (request.method !== "GET" && request.method !== "HEAD") {
    init.body = await request.text();
  }

  let upstream: Response;
  try {
    upstream = await fetch(target, init);
  } catch (cause) {
    // The engine being down is the single most likely failure in a fresh
    // deployment, so it gets a message that says what to do about it.
    return Response.json(
      {
        error: `cannot reach the Asaborake engine at ${ENGINE}`,
        hint: "is `asaborake serve` running?",
        cause: String(cause),
      },
      { status: 502 },
    );
  }

  // Server-sent events are streamed straight through; buffering them here
  // would turn a live stream into a stalled one.
  if (upstream.headers.get("content-type")?.includes("text/event-stream")) {
    return new Response(upstream.body, {
      status: upstream.status,
      headers: {
        "content-type": "text/event-stream",
        "cache-control": "no-cache",
        connection: "keep-alive",
        // Proxies buffer by default; this is the conventional way to ask
        // them not to.
        "x-accel-buffering": "no",
      },
    });
  }

  return upstream;
}

/**
 * Resolve a request path to a file inside the client build.
 *
 * Returns `null` for anything that escapes the build directory. The check is
 * on the *resolved* path rather than on the request string, because `%2e%2e`
 * and its relatives only become traversal after decoding.
 */
function clientFile(pathname: string): string | null {
  const resolved = new URL(`.${pathname}`, `file://${CLIENT_ROOT}`).pathname;
  return resolved.startsWith(CLIENT_ROOT) ? resolved : null;
}

/** Serve the built client, or its shell for a route the router owns. */
async function serveClient(pathname: string): Promise<Response> {
  if (!SERVE_CLIENT) {
    return Response.json({ error: "not found" }, { status: 404 });
  }

  const path = clientFile(pathname);
  if (path && pathname !== "/") {
    const file = Bun.file(path);
    if (await file.exists()) return new Response(file);
  }

  // A deep link such as /jobs/abc has no file behind it; the router resolves
  // it in the browser, so the shell is returned instead of a 404.
  return new Response(Bun.file(`${CLIENT_ROOT}index.html`), {
    headers: { "content-type": "text/html; charset=utf-8" },
  });
}

export const app = new Elysia()
  .all("/*", async ({ request }) => {
    const { pathname } = new URL(request.url);

    if (pathname === "/healthz") {
      return Response.json({ status: "ok", engine: ENGINE });
    }
    if (pathname.startsWith("/api/")) {
      return forward(request, pathname);
    }
    return serveClient(pathname);
  })
  .onError(({ error }) => {
    console.error("web server error", error);
    return Response.json({ error: "internal error" }, { status: 500 });
  });

if (import.meta.main) {
  app.listen(PORT);
  console.log(`Asaborake web on http://localhost:${PORT} (engine: ${ENGINE})`);
}

export type App = typeof app;
