/**
 * Qingluan workspace session switching for Pi (`/ws`).
 *
 * Lists the JJ workspaces of the current repository plus their Pi sessions by
 * shelling out to `qingluan workspace list --json` (run in `ctx.cwd`), shows a
 * flat selector (labels built by the pure `./catalog` helper), and:
 *
 * - selecting a session resumes it via `ctx.switchSession(file)` — the session
 *   header's cwd moves the agent to that workspace;
 * - selecting "✚ new session" in the current workspace starts a fresh session
 *   via `ctx.newSession()`;
 * - selecting "✚ new session" in another workspace carries the current
 *   conversation over with `SessionManager.forkFrom(currentFile, targetRoot)`
 *   and then switches to the fork;
 * - unavailable sessions remain visible as `×` rows; selecting one only
 *   notifies the missing-workspace reason and reopens the selector.
 *
 * Guards: no dialog-capable UI → notify and stop; ephemeral (unpersisted)
 * current session → cannot be forked across workspaces. Esc cancels silently.
 */
import { resolve as resolvePath, sep } from "node:path";
import {
  SessionManager,
  type ExtensionAPI,
  type ExtensionCommandContext,
} from "@earendil-works/pi-coding-agent";
import {
  CATALOG_SCHEMA_VERSION,
  buildOptions,
  samePath,
  type SelectableTarget,
  type WorkspaceCatalog,
} from "./catalog";

export default function qingluanWorkspace(pi: ExtensionAPI): void {
  pi.registerCommand("ws", {
    description: "Switch to a Pi session in a Qingluan JJ workspace",
    handler: async (_args: string, ctx: ExtensionCommandContext): Promise<void> => {
      await switchWorkspace(pi, ctx);
    },
  });
}

async function switchWorkspace(
  pi: ExtensionAPI,
  ctx: ExtensionCommandContext,
): Promise<void> {
  // Guard: the selector needs dialog-capable UI.
  if (!ctx.hasUI) {
    ctx.ui.notify("/ws requires an interactive UI session", "warning");
    return;
  }

  const catalog = await fetchCatalog(pi, ctx);
  if (!catalog) return; // failure already notified

  const currentFile = ctx.sessionManager.getSessionFile();
  const cwd = resolvePath(ctx.cwd);

  const { labels, targets } = buildOptions(catalog, currentFile);
  if (labels.length === 0) {
    ctx.ui.notify("No Qingluan workspaces in this repository", "warning");
    return;
  }

  for (;;) {
    const choice = await ctx.ui.select("Qingluan workspace session", labels);
    if (choice === undefined) return; // Esc
    const target = targets[labels.indexOf(choice)];
    if (!target) return;
    if (target.kind === "unavailable") {
      // Informational row: show why the workspace is unusable, then reopen
      // the selector so the user can pick something else.
      ctx.ui.notify(`× ${target.name}: ${target.reason}`, "warning");
      continue;
    }
    await switchToTarget(ctx, target, currentFile, cwd);
    return;
  }
}

async function switchToTarget(
  ctx: ExtensionCommandContext,
  target: SelectableTarget,
  currentFile: string | undefined,
  cwd: string,
): Promise<void> {
  try {
    if (target.kind === "resume") {
      if (currentFile !== undefined && samePath(target.file, currentFile)) {
        ctx.ui.notify("Already in this session", "info");
        return;
      }
      await ctx.switchSession(target.file);
      return;
    }

    if (isInsideOrEqual(cwd, resolvePath(target.root))) {
      // Same workspace: no transition, a plain new session is enough.
      await ctx.newSession();
      return;
    }

    // Guard: an ephemeral current session has no file to fork from.
    if (currentFile === undefined) {
      ctx.ui.notify(
        "Current session is ephemeral; it cannot be carried to another workspace",
        "warning",
      );
      return;
    }
    // Cross-workspace new: carry the current discussion over.
    const forked = SessionManager.forkFrom(currentFile, target.root);
    const forkedFile = forked.getSessionFile();
    if (forkedFile === undefined) {
      ctx.ui.notify("Forked session was not persisted", "error");
      return;
    }
    await ctx.switchSession(forkedFile);
  } catch (error) {
    ctx.ui.notify(`Workspace switch failed: ${errorMessage(error)}`, "error");
  }
}

/** Run `qingluan workspace list --json` in ctx.cwd and validate the payload. */
async function fetchCatalog(
  pi: ExtensionAPI,
  ctx: ExtensionCommandContext,
): Promise<WorkspaceCatalog | undefined> {
  const result = await pi.exec("qingluan", ["workspace", "list", "--json"], {
    cwd: ctx.cwd,
  });
  if (result.code !== 0) {
    ctx.ui.notify(
      `qingluan workspace list failed: ${firstLine(result.stderr)}`,
      "error",
    );
    return undefined;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(result.stdout);
  } catch {
    ctx.ui.notify("qingluan workspace list returned invalid JSON", "error");
    return undefined;
  }
  if (
    typeof parsed !== "object" ||
    parsed === null ||
    (parsed as { schemaVersion?: unknown }).schemaVersion !== CATALOG_SCHEMA_VERSION ||
    !Array.isArray((parsed as { workspaces?: unknown }).workspaces)
  ) {
    ctx.ui.notify(
      "Unexpected qingluan workspace catalog (unsupported schemaVersion or shape)",
      "error",
    );
    return undefined;
  }
  return parsed as WorkspaceCatalog;
}

function isInsideOrEqual(child: string, parent: string): boolean {
  return child === parent || child.startsWith(parent + sep);
}

function firstLine(text: string): string {
  return text.trim().split("\n")[0] ?? "";
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
