/**
 * Pure catalog→selector mapping for `/ws`.
 *
 * Split out of the extension command so it stays unit-testable with Node's
 * built-in test runner (type stripping) without the Pi runtime: this module
 * must not import anything beyond `node:path` at runtime.
 */
import { resolve as resolvePath } from "node:path";

/** One session of a workspace in the Qingluan catalog. */
export interface WorkspaceSession {
  file: string;
  title: string;
  messageCount: number;
  modified: string;
}

/** One workspace in the Qingluan catalog. */
export interface WorkspaceEntry {
  name: string;
  root: string;
  available: boolean;
  unavailableReason?: string | null;
  sessions: WorkspaceSession[];
}

/** Machine-readable `qingluan workspace list --json` payload. */
export interface WorkspaceCatalog {
  schemaVersion: number;
  workspaces: WorkspaceEntry[];
}

/** What the user picked from the flat selector. */
export type Target =
  | { kind: "resume"; file: string }
  | { kind: "new"; root: string }
  | { kind: "unavailable"; name: string; reason: string };

/** A target that performs an action (everything except the `×` info rows). */
export type SelectableTarget = Exclude<Target, { kind: "unavailable" }>;

/** Flat selector labels and their targets. */
export interface SelectorOptions {
  labels: string[];
  targets: Target[];
}

export const CATALOG_SCHEMA_VERSION = 1;

const TITLE_MAX_CHARS = 80;

/**
 * Build the flat selector options.
 *
 * Available workspaces contribute their sessions plus one `✚ new session`
 * entry; the session matching `currentFile` (when given) is marked
 * `·current`. An unavailable workspace contributes one informational row per
 * known session so its history stays visible; if it has no sessions, the
 * workspace itself contributes one row. None can be resumed while the root is
 * missing. Labels are globally unique.
 */
export function buildOptions(
  catalog: WorkspaceCatalog,
  currentFile?: string,
): SelectorOptions {
  const labels: string[] = [];
  const targets: Target[] = [];
  const seen = new Set<string>();
  const push = (label: string, target: Target): void => {
    let unique = label;
    for (let n = 2; seen.has(unique); n++) unique = `${label} (#${n})`;
    seen.add(unique);
    labels.push(unique);
    targets.push(target);
  };

  for (const ws of catalog.workspaces) {
    if (!ws.available) {
      const reason = ws.unavailableReason ?? "unavailable";
      if (ws.sessions.length === 0) {
        push(`${ws.name} ── × ${reason}`, {
          kind: "unavailable",
          name: ws.name,
          reason,
        });
      } else {
        for (const session of ws.sessions) {
          push(
            `${ws.name} ── × ${displayTitle(session.title)} (${session.messageCount} msgs, ${session.modified}) [${reason}]`,
            { kind: "unavailable", name: ws.name, reason },
          );
        }
      }
      continue;
    }
    for (const session of ws.sessions) {
      const active =
        currentFile !== undefined && samePath(session.file, currentFile);
      push(
        `${ws.name} ── ${displayTitle(session.title)} (${session.messageCount} msgs, ${session.modified})${active ? " ·current" : ""}`,
        { kind: "resume", file: session.file },
      );
    }
    push(`${ws.name} ── ✚ new session`, { kind: "new", root: ws.root });
  }

  return { labels, targets };
}

/** Collapse a session title into one short display line. */
export function displayTitle(title: string): string {
  const singleLine = title.split(/\s+/).filter(Boolean).join(" ");
  if (singleLine.length <= TITLE_MAX_CHARS) return singleLine;
  return `${singleLine.slice(0, TITLE_MAX_CHARS - 1)}…`;
}

/** Compare two paths after lexical normalization. */
export function samePath(a: string, b: string): boolean {
  return resolvePath(a) === resolvePath(b);
}
