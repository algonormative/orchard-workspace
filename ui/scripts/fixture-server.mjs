import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";

const port = Number(process.env.ORCHARD_FIXTURE_PORT || 4174);
const dist = new URL("../dist/", import.meta.url);
const workspace = { id: "workspace-1", name: "Fixture workspace" };
const archivedWorkspace = { id: "archived-first", name: "Archived fixture", archived: true };
const taskStore = { id: "default", name: "Workspace tasks", source: "owned", path: "/private/tmp/orchard-fixture-workspaces/a-very-long-workspace-identifier-that-must-wrap-within-the-details-panel/tasks" };
const projectStore = { id: "repository:example-project", name: "example-project", source: "repository", repository_id: "project-example", path: "/private/tmp/example-project/.beads" };
let repositories = [];
const participants = [
  { id: "owner", name: "Owner" },
  { id: "alice", name: "Alice", last_contact_at: "2026-09-15T12:00:00Z" },
];
const channels = [{ id: "general", name: "general" }, { id: "orchard-system", name: "orchard-system" }];
let created = false;
let workspaces = [];
let sessionsValid = true;
let sourceErrors = [];
let taskBackendAvailable = true;
let delaySendMs = 0;
let delayAttachMs = 0;
let delayTasksMs = 0;
let delayActionMs = 0;
let resourceLinks = [];
let tasks = [{ id: "fixture-1", task_id: "fixture-1", title: "Fixture task", status: "open", description: "Fixture task description" }];
const messages = [
  { id: "general-1", sender_id: "alice", destination: { kind: "channel", id: "general" }, body: "General fixture message\n```sh\nprintf 'fixture code'\n```", kind: "message" },
  { id: "direct-1", sender_id: "owner", destination: { kind: "direct", id: "alice" }, body: "Owner to Alice", kind: "message" },
  { id: "direct-2", sender_id: "alice", destination: { kind: "direct", id: "owner" }, body: "Alice to Owner", kind: "message" },
  { id: "direct-3", sender_id: "alice", destination: { kind: "direct", id: "orchard" }, body: "Agent to agent", kind: "decision" },
  { id: "broadcast-1", sender_id: "owner", destination: { kind: "broadcast" }, body: "Broadcast fixture message", kind: "result" },
];
const calls = [];
const artifactFiles = {
  "README.md": { text: "# Fixture heading\n\n**Bold** text.\n\n- [Code](docs/example.py)\n- [Outside](../../escape.txt)\n\n```js\nconsole.log('fixture')\n```", mime_type: "text/markdown", download_url: "/fixture/download/README.md" },
  "docs/example.py": { text: "print('fixture')\n", mime_type: "text/x-python" },
  "image.png": { text: null, binary: true, byte_length: 4, mime_type: "image/png", preview_url: "/fixture/image.png", download_url: "/fixture/download/image.png" },
  "empty.txt": { text: "", byte_length: 0, mime_type: "text/plain", download_url: "/fixture/download/empty.txt" },
};
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function resetFixture() {
  created = false; workspaces = []; sessionsValid = true; sourceErrors = []; taskBackendAvailable = true;
  repositories = []; delaySendMs = 0; delayAttachMs = 0; delayTasksMs = 0;
  delayActionMs = 0; resourceLinks = [];
  tasks = [{ id: "fixture-1", task_id: "fixture-1", title: "Fixture task", status: "open", description: "Fixture task description" }];
  calls.splice(0, calls.length);
  messages.splice(5);
}

const send = (response, status, body, headers = {}) => response.writeHead(status, { "content-type": "application/json", ...headers }).end(JSON.stringify(body));
const bodyOf = async (request) => new Promise((resolve, reject) => {
  let body = "";
  request.on("data", (chunk) => { body += chunk; });
  request.on("end", () => { try { resolve(JSON.parse(body || "{}")); } catch (error) { reject(error); } });
});

function snapshot(workspaceId = workspace.id) {
  const stores = [{ store: taskStore, tasks }, ...repositories.filter((repository) => repository.task_store_id).map(() => ({ store: projectStore, tasks: [] }))];
  const selected = workspaces.find((item) => item.id === workspaceId) || workspace;
  return { workspace: { ...selected, repositories, task_stores: stores.map((item) => item.store) }, mail: { participants, channels, history: messages }, task_stores: stores, errors: sourceErrors };
}

function history(args) {
  let result = messages.filter((entry) => !args.destination_kind || entry.destination.kind === args.destination_kind);
  if (args.channel_id) result = result.filter((entry) => entry.destination.id === args.channel_id);
  return { messages: result };
}

async function staticFile(pathname, response) {
  const requested = pathname === "/" ? "index.html" : pathname.slice(1);
  const safe = normalize(requested).replace(/^\.\.([/\\]|$)/, "");
  try {
    const contents = await readFile(new URL(safe, dist));
    const type = extname(safe) === ".js" ? "text/javascript" : extname(safe) === ".css" ? "text/css" : "text/html";
    response.writeHead(200, { "content-type": type }).end(contents);
  } catch {
    response.writeHead(200, { "content-type": "text/html" }).end(await readFile(new URL("index.html", dist)));
  }
}

const server = createServer(async (request, response) => {
  const url = new URL(request.url, `http://${request.headers.host}`);
  const authenticated = sessionsValid && (request.headers.cookie?.includes("orchard_session=fixture") || false);
  if (url.pathname === "/api/session" && request.method === "GET") return send(response, 200, { authenticated });
  if (url.pathname === "/api/session" && request.method === "POST") {
    const payload = await bodyOf(request);
    if (payload.token !== "fixture-access-key") return send(response, 401, { error: "The access key was not accepted." });
    sessionsValid = true;
    return send(response, 200, { authenticated: true }, { "set-cookie": "orchard_session=fixture; HttpOnly; SameSite=Strict; Path=/api" });
  }
  if (url.pathname === "/api/call" && request.method === "POST") {
    if (!authenticated) return send(response, 401, { error: "Authentication required." });
    const payload = await bodyOf(request);
    const args = payload.args || {};
    calls.push({ operation: payload.operation, args });
    if (payload.operation === "workspace_list") return send(response, 200, { result: { workspaces: [archivedWorkspace, ...workspaces] } });
    if (payload.operation === "workspace_create") { created = true; const createdWorkspace = { id: `workspace-${workspaces.length + 1}`, name: args.name || `Workspace ${workspaces.length + 1}` }; workspaces.push(createdWorkspace); return send(response, 200, { result: { workspace: createdWorkspace } }); }
    if (payload.operation === "workspace_archive") { const selected = workspaces.find((item) => item.id === args.workspace_id) || workspace; workspaces = workspaces.filter((item) => item.id !== args.workspace_id); created = workspaces.length > 0; return send(response, 200, { result: { workspace: { ...selected, archived: true } } }); }
    if (payload.operation === "workspace_snapshot") return send(response, 200, { result: snapshot(args.workspace_id) });
    if (payload.operation === "resource_get") {
      const ref = args.ref || {}; const kind = ref.kind;
      const descriptor = (title, data) => ({ ref, href: `/w/${encodeURIComponent(args.workspace_id)}/${kind === "channel" ? "channels" : kind === "direct" ? "direct" : kind === "broadcast" ? "broadcast" : `${kind}s`}/${ref.id || ""}`.replace(/\/$/, ""), title, kind, data });
      const links = { outgoing: resourceLinks.filter((entry) => JSON.stringify(entry.source) === JSON.stringify(ref)), incoming: resourceLinks.filter((entry) => JSON.stringify(entry.target) === JSON.stringify(ref)) };
      if (kind === "channel") return send(response, 200, { result: { resource: descriptor(ref.id, { channel: channels.find((entry) => entry.id === ref.id), messages: messages.filter((entry) => entry.destination.kind === "channel" && entry.destination.id === ref.id) }), links } });
      if (kind === "direct") return send(response, 200, { result: { resource: descriptor(ref.id, { participant: participants.find((entry) => entry.id === ref.id), messages: messages.filter((entry) => entry.destination.kind === "direct") }), links } });
      if (kind === "broadcast") return send(response, 200, { result: { resource: descriptor("Broadcast", { messages: messages.filter((entry) => entry.destination.kind === "broadcast") }), links } });
      if (kind === "message") { const record = messages.find((entry) => entry.id === ref.id); return record ? send(response, 200, { result: { resource: descriptor("Message", { message: record }), links } }) : send(response, 404, { error: "Message not found" }); }
      if (kind === "agent") return send(response, 200, { result: { resource: descriptor(ref.id, { participant: participants.find((entry) => entry.id === ref.id) }), links } });
      if (kind === "task") { const task = tasks.find((entry) => entry.id === ref.task_id); return task ? send(response, 200, { result: { resource: descriptor(task.title, { task, dependencies: [] }), links } }) : send(response, 404, { error: "Task not found" }); }
      if (kind === "file") { const file = artifactFiles[ref.path]; return file ? send(response, 200, { result: { resource: descriptor(ref.path, file), links } }) : send(response, 404, { error: "File not found" }); }
      if (kind === "url") return send(response, 200, { result: { resource: descriptor(ref.url, { url: ref.url }), links } });
    }
    if (payload.operation === "artifact_roots") return send(response, 200, { result: { roots: [{ id: "fixture-root", name: "Fixture artifacts", owned: true, exists: true }] } });
    if (payload.operation === "artifact_list") { const path = args.path || ""; const entries = path ? [{ name: "example.py", path: "docs/example.py", kind: "file" }, { name: "module", path: "docs/module", kind: "submodule" }] : [{ name: "README.md", path: "README.md", kind: "file" }, { name: "docs", path: "docs", kind: "directory" }, { name: "image.png", path: "image.png", kind: "file" }, { name: "empty.txt", path: "empty.txt", kind: "file" }]; return send(response, 200, { result: { entries } }); }
    if (payload.operation === "artifact_history") return send(response, 200, { result: { versions: [{ revision: "0123456789abcdef0123456789abcdef01234567", summary: "Fixture version" }] } });
    if (payload.operation === "resource_link") { if (delayActionMs) await sleep(delayActionMs); const link = { source: args.source, target: args.target, label: args.label || "" }; resourceLinks.push(link); return send(response, 200, { result: { link } }); }
    if (payload.operation === "artifact_upload") return send(response, 200, { result: { resource: { ref: { kind: "file", workspace_id: args.workspace_id, root_id: "fixture-root", path: args.path }, href: `/w/${args.workspace_id}/files/fixture-root?path=${encodeURIComponent(args.path)}`, title: args.path, kind: "file" }, revision: "fixture" } });
    if (payload.operation === "repository_attach") {
      if (delayAttachMs) await sleep(delayAttachMs);
      const isPlain = args.path.includes("plain");
      const repository = { id: `project-${repositories.length + 1}`, path: args.path, name: args.path.split("/").filter(Boolean).at(-1), task_store_id: isPlain ? null : projectStore.id, task_status: isPlain ? "none" : "linked", task_error: null };
      repositories = [...repositories, repository];
      return send(response, 200, { result: { repository, task_store: isPlain ? null : projectStore, attached: true, task_store_attached: !isPlain } });
    }
    if (payload.operation === "tasks_list") { if (delayTasksMs) await sleep(delayTasksMs); return send(response, 200, { result: { tasks } }); }
    if (payload.operation === "task_create") { const task = { id: `fixture-${tasks.length + 1}`, task_id: `fixture-${tasks.length + 1}`, title: args.title, status: "open", description: "" }; tasks.push(task); return send(response, 200, { result: { task } }); }
    if (payload.operation === "task_update") { if (delayActionMs) await sleep(delayActionMs); const task = tasks.find((entry) => entry.id === args.task_id); if (task) task.status = args.status; return send(response, 200, { result: { task } }); }
    if (payload.operation === "task_close") { const task = tasks.find((entry) => entry.id === args.task_id); if (task) task.status = "closed"; return send(response, 200, { result: { task } }); }
    if (payload.operation === "task_show") return send(response, 200, { result: { task: tasks.find((entry) => entry.id === args.task_id) } });
    if (payload.operation === "task_dependencies") return send(response, 200, { result: { dependencies: [] } });
    if (payload.operation === "mail_history") return send(response, 200, { result: history(args) });
    if (payload.operation === "mail_send") { if (delaySendMs) await sleep(delaySendMs); messages.push({ id: `sent-${messages.length}`, sender_id: args.sender_id, destination: args.destination, body: args.body, kind: args.kind, thread_id: args.thread_id, refs: args.refs }); return send(response, 200, { result: { message: messages.at(-1) } }); }
    if (payload.operation === "connection_info") return send(response, 200, { result: { endpoint: "http://127.0.0.1:4174/workspaces/workspace-1/mcp", token: "fixture-mcp-token" } });
    if (payload.operation === "settings_get") return send(response, 200, { result: { config: { task_backend: taskBackendAvailable ? { available: true } : { available: false, error: "Fixture Beads binary is unavailable" } } } });
    return send(response, 200, { result: {} });
  }
  if (url.pathname === "/fixture/audit") return send(response, 200, { calls, messages });
  if (url.pathname === "/fixture/long-history" && request.method === "POST") {
    for (let index = 0; index < 40; index += 1) messages.push({ id: `long-${index}`, sender_id: "alice", destination: { kind: "channel", id: "general" }, body: `Long history ${index}`, kind: "message" });
    return send(response, 200, { ok: true });
  }
  if (url.pathname === "/fixture/reset" && request.method === "POST") { resetFixture(); return send(response, 200, { ok: true }); }
  if (url.pathname === "/fixture/delay" && request.method === "POST") { const value = await bodyOf(request); delaySendMs = Number(value.send || 0); delayAttachMs = Number(value.attach || 0); delayTasksMs = Number(value.tasks || 0); delayActionMs = Number(value.action || 0); return send(response, 200, { ok: true }); }
  if (url.pathname === "/fixture/revoke" && request.method === "POST") { sessionsValid = false; return send(response, 200, { revoked: true }); }
  if (url.pathname === "/fixture/source-error" && request.method === "POST") { sourceErrors = [{ source: "task_store", error: "Fixture backend is unavailable" }]; return send(response, 200, { errors: sourceErrors }); }
  if (url.pathname === "/fixture/task-backend-down" && request.method === "POST") { taskBackendAvailable = false; return send(response, 200, { available: false }); }
  return staticFile(url.pathname, response);
});

server.listen(port, "127.0.0.1", () => console.log(`Orchard fixture http://127.0.0.1:${port} (access key: fixture-access-key)`));
