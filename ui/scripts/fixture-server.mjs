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
let sessionsValid = true;
let sourceErrors = [];
let taskBackendAvailable = true;
let delaySendMs = 0;
let delayAttachMs = 0;
let delayTasksMs = 0;
const messages = [
  { id: "general-1", sender_id: "alice", destination: { kind: "channel", id: "general" }, body: "General fixture message\n```sh\nprintf 'fixture code'\n```", kind: "message" },
  { id: "direct-1", sender_id: "owner", destination: { kind: "direct", id: "alice" }, body: "Owner to Alice", kind: "message" },
  { id: "direct-2", sender_id: "alice", destination: { kind: "direct", id: "owner" }, body: "Alice to Owner", kind: "message" },
  { id: "direct-3", sender_id: "alice", destination: { kind: "direct", id: "orchard" }, body: "Agent to agent", kind: "decision" },
  { id: "broadcast-1", sender_id: "owner", destination: { kind: "broadcast" }, body: "Broadcast fixture message", kind: "result" },
];
const calls = [];
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function resetFixture() {
  created = false; sessionsValid = true; sourceErrors = []; taskBackendAvailable = true;
  repositories = []; delaySendMs = 0; delayAttachMs = 0; delayTasksMs = 0;
  calls.splice(0, calls.length);
  messages.splice(5);
}

const send = (response, status, body, headers = {}) => response.writeHead(status, { "content-type": "application/json", ...headers }).end(JSON.stringify(body));
const bodyOf = async (request) => new Promise((resolve, reject) => {
  let body = "";
  request.on("data", (chunk) => { body += chunk; });
  request.on("end", () => { try { resolve(JSON.parse(body || "{}")); } catch (error) { reject(error); } });
});

function snapshot() {
  const stores = [{ store: taskStore, tasks: [] }, ...repositories.filter((repository) => repository.task_store_id).map(() => ({ store: projectStore, tasks: [] }))];
  return { workspace: { ...workspace, repositories, task_stores: stores.map((item) => item.store) }, mail: { participants, channels, history: messages }, task_stores: stores, errors: sourceErrors };
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
    if (payload.operation === "workspace_list") return send(response, 200, { result: { workspaces: created ? [archivedWorkspace, workspace] : [] } });
    if (payload.operation === "workspace_create") { created = true; return send(response, 200, { result: { workspace } }); }
    if (payload.operation === "workspace_archive") { created = false; return send(response, 200, { result: { workspace: { ...workspace, archived: true } } }); }
    if (payload.operation === "workspace_snapshot") return send(response, 200, { result: snapshot() });
    if (payload.operation === "repository_attach") {
      if (delayAttachMs) await sleep(delayAttachMs);
      const isPlain = args.path.includes("plain");
      const repository = { id: `project-${repositories.length + 1}`, path: args.path, name: args.path.split("/").filter(Boolean).at(-1), task_store_id: isPlain ? null : projectStore.id, task_status: isPlain ? "none" : "linked", task_error: null };
      repositories = [...repositories, repository];
      return send(response, 200, { result: { repository, task_store: isPlain ? null : projectStore, attached: true, task_store_attached: !isPlain } });
    }
    if (payload.operation === "tasks_list") { if (delayTasksMs) await sleep(delayTasksMs); return send(response, 200, { result: { tasks: [] } }); }
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
  if (url.pathname === "/fixture/delay" && request.method === "POST") { const value = await bodyOf(request); delaySendMs = Number(value.send || 0); delayAttachMs = Number(value.attach || 0); delayTasksMs = Number(value.tasks || 0); return send(response, 200, { ok: true }); }
  if (url.pathname === "/fixture/revoke" && request.method === "POST") { sessionsValid = false; return send(response, 200, { revoked: true }); }
  if (url.pathname === "/fixture/source-error" && request.method === "POST") { sourceErrors = [{ source: "task_store", error: "Fixture backend is unavailable" }]; return send(response, 200, { errors: sourceErrors }); }
  if (url.pathname === "/fixture/task-backend-down" && request.method === "POST") { taskBackendAvailable = false; return send(response, 200, { available: false }); }
  return staticFile(url.pathname, response);
});

server.listen(port, "127.0.0.1", () => console.log(`Orchard fixture http://127.0.0.1:${port} (access key: fixture-access-key)`));
