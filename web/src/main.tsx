/**
 * The Asaborake web app.
 *
 * A left rail of functions, as on the front panel of a piece of hardware, and
 * one dense view per function. There are four things an operator does — watch
 * the queue, inspect one job, check what logos the machine has learned, and
 * see what it can encode with — so there are four routes.
 */

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import {
  Link,
  Outlet,
  RouterProvider,
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";

import "./styles.css";
import { Dashboard } from "./routes/dashboard";
import { JobDetail } from "./routes/job";
import { Logos } from "./routes/logos";
import { Profiles } from "./routes/profiles";

const rootRoute = createRootRoute({ component: Shell });

function Shell() {
  return (
    <div className="flex h-full">
      <nav className="flex w-44 shrink-0 flex-col border-r border-rule bg-panel">
        <div className="border-b border-rule px-4 py-5">
          <div className="text-[15px] tracking-[0.12em] text-programme">
            ASABORAKE
          </div>
          <div className="eyebrow mt-1.5">朝ぼらけ</div>
        </div>

        <div className="flex flex-col py-2">
          <RailLink to="/" label="Queue" />
          <RailLink to="/logos" label="Logos" />
          <RailLink to="/profiles" label="Profiles" />
        </div>

        <div className="mt-auto border-t border-rule px-4 py-4 text-[10px] leading-relaxed text-ink-faint">
          Inspired by{" "}
          <a
            href="https://github.com/nekopanda/Amatsukaze"
            className="text-ink-dim underline decoration-rule-bright underline-offset-2 hover:text-logo"
            target="_blank"
            rel="noreferrer"
          >
            Amatsukaze
          </a>
        </div>
      </nav>

      <main className="flex-1 overflow-y-auto">
        <Outlet />
      </main>
    </div>
  );
}

function RailLink({ to, label }: { to: string; label: string }) {
  return (
    <Link
      to={to}
      className="px-4 py-2 text-ink-dim transition-colors hover:bg-raised hover:text-ink"
      activeProps={{
        // A lit indicator on the active function, as a hardware panel has.
        className:
          "px-4 py-2 text-ink bg-raised border-l-2 border-programme -ml-[2px] pl-[calc(1rem+2px)]",
      }}
      activeOptions={{ exact: to === "/" }}
    >
      {label}
    </Link>
  );
}

const routeTree = rootRoute.addChildren([
  createRoute({ getParentRoute: () => rootRoute, path: "/", component: Dashboard }),
  createRoute({
    getParentRoute: () => rootRoute,
    path: "/jobs/$jobId",
    component: JobDetail,
  }),
  createRoute({
    getParentRoute: () => rootRoute,
    path: "/logos",
    component: Logos,
  }),
  createRoute({
    getParentRoute: () => rootRoute,
    path: "/profiles",
    component: Profiles,
  }),
]);

const router = createRouter({ routeTree, defaultPreload: "intent" });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}

const container = document.getElementById("root");
if (container) {
  createRoot(container).render(
    <StrictMode>
      <RouterProvider router={router} />
    </StrictMode>,
  );
}
