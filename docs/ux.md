# Orchard Workspace browser UX constraints

Start with a valid empty workspace. The first useful action is creating it;
connection configuration and technical records live in Settings or a selected
object, never in an empty dashboard.

The visible workspace grows from observed records. A channel selection reveals
its conversation and composer. A selected task store reveals tasks. A selected
task reveals task details. The shell shows unavailable services as errors and
does not manufacture example participants, messages, tasks, or ledger entries.

The composer is a continuous working surface. Polling updates only the selected
conversation's scroll container and preserves a focused draft, reply context,
and reading position. A direct view shows the owner↔participant exchange; an
All direct messages view exposes owner-observable agent-to-agent routing with
sender and recipient attribution. Broadcast is its own readable conversation.

Tasks, People, Activity, and attachment forms open only when selected. Activity
loads all messages and task receipts across the workspace; its plain text search
covers messages and references without asking people to classify them. References
remain readable records. The internal `orchard` participant and
`task-receipts`/`orchard-system` channels do not crowd ordinary conversations.
Snapshot source errors and unavailable task stores appear as errors instead of
empty content. Actions have text labels and errors have text feedback; color is
supplementary.

Settings exposes a per-workspace endpoint, credential copy actions, and
credential rotation. It explains that Orchard connects already-running agents:
the harness must reload its own MCP configuration, register or resume an
identity, poll its inbox, and acknowledge messages. It does not expose a
protocol log dashboard.

All code-like blocks use one shared renderer with text-node content, a visible
generic Copy control, and contained horizontal scrolling. This preserves the
exact copied source while keeping long commands, endpoints, tokens, and fenced
message snippets from widening the page. Secondary views have a visible exit,
Escape support, and browser Back returns to the prior in-app context. Polling
updates data without replacing an active form, task editor, draft, reply state,
focus, or reader scroll position.

Tasks open to the workspace-owned store first. Projects are added by path and
shown by their basename; a project without a usable Beads store remains visible
with a plain status instead of being initialized automatically. Legacy external
stores stay available under Other task sources.
