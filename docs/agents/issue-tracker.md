# Issue tracker: Plane

Issues and specs for this repo live in the Plane project **QINGLUAN** (id
`eadf46e8-a81b-4451-994b-5d3a03c8c05b`). All operations go through the `plane`
MCP server's tools — there is no CLI.

## Conventions

- **Create an issue**: `plane_workitem` action `create` with `project_id` and
  `name`; body goes in `description_stripped` (plain text) or
  `description_html`. Labels/states take UUIDs — `plane_label` / `plane_state`
  `list` first if you only have names.
- **Read an issue**: `plane_workitem` `retrieve_by_identifier` with
  `workitem_identifier` like `QINGLUAN-42`; expand comments via
  `plane_workitem_comment` `list`.
- **List issues**: `plane_workitem` `list` with `project_id`, optionally
  filtered with `pql` (syntax: `plane_get_pql_reference`).
- **Comment**: `plane_workitem_comment` `create`.
- **Apply / remove labels**: `plane_workitem` `manage_label` with
  `add_label_id` / `remove_label_id` (merges; removals apply first).
- **Close**: `plane_workitem` `update` with `state` set to the UUID of the
  Done (`completed`) state. Cancelled issues use the `cancelled` state.

## States

The project uses Plane's default workflow: Backlog (backlog, default), Todo
(unstarted), In Progress (started), Done (completed), Cancelled (cancelled).

## When a skill says "publish to the issue tracker"

Create a Plane work item in project QINGLUAN.

## When a skill says "fetch the relevant ticket"

`plane_workitem` `retrieve_by_identifier` + `plane_workitem_comment` `list`.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a single work item with **child** work
items as tickets.

- **Map**: one work item labelled `wayfinder:map`, holding the Notes /
  Decisions-so-far / Fog in its description.
- **Child ticket**: a work item whose `parent` is the map. Labels:
  `wayfinder:<type>` (`research`/`prototype`/`grilling`/`task`). Once claimed,
  the ticket is assigned to the driving dev.
- **Blocking**: `plane_workitem_relation` — add a `blocked_by` relation from
  child to blocker. A ticket is unblocked when every blocker is in a
  `completed` state.
- **Frontier query**: list the map's open children (`plane_workitem` `list`,
  PQL on parent + state), drop any with an open blocker or an assignee; first
  in sort order wins.
- **Claim**: `plane_workitem` `manage_assignee` `add_user_id` — the session's
  first write.
- **Resolve**: comment the answer, move state to Done, then append a context
  pointer to the map's Decisions-so-far (map description edit).
