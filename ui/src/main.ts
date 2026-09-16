import "./style.css";
import { canonicalHref, parseHref, type Descriptor, type ResourceRef } from "./resources";

type Json = Record<string, unknown>;
type Workspace = { id: string; name: string; archived?: boolean };
type Store = { id: string; path?: string; name?: string; source?: string };
type ConversationKind = "channel" | "direct" | "broadcast";
type Draft = { body: string; reference: string };
type DetailView = "form";
type Screen = "workspace" | "settings" | "new-workspace" | "home";
type CollectionTab = { kind: "collection"; collection: "tasks" | "agents" | "directs"; workspaceId: string; href: string; title: string };
type AppTab = Descriptor | CollectionTab;

const rootElement = document.querySelector<HTMLElement>("#app");
if (!rootElement) throw new Error("Orchard could not find its app container.");
const root: HTMLElement = rootElement;

const state: {
  workspaces: Workspace[];
  workspace?: Workspace;
  snapshot?: Json;
  taskBackend?: Json;
  store?: Store;
  selectedTask?: string;
  selectedConversation?: string;
  conversationKind: ConversationKind;
  conversationMessages: unknown[];
  drafts: Map<string, Draft>;
  workspaceRequest: number;
  conversationRequest: number;
  seenMessageIds: Set<string>;
  unread: Map<string, number>;
  senderId?: string;
  replyTo?: string;
  detailView?: DetailView;
  screen: Screen;
  detailEpoch: number;
  taskRequest: number;
  threadScroll?: number;
  composerFocused?: boolean;
  poll?: number;
  tabs: AppTab[];
  activeHref?: string;
  resourceRequest: number;
  activeResource?: Descriptor;
  resourceData?: Json;
  resourceLinks?: Json;
  navigationEpoch: number;
  treeExpanded: Set<"chats" | "tasks" | "artifacts">;
  artifactRoots: Json[];
  artifactEntries: Map<string, Json[]>;
  artifactExpanded: Set<string>;
  formReturn?: AppTab;
} = { workspaces: [], conversationKind: "channel", conversationMessages: [], drafts: new Map(), workspaceRequest: 0, conversationRequest: 0, seenMessageIds: new Set(), unread: new Map(), screen: "workspace", detailEpoch: 0, taskRequest: 0, tabs: [], resourceRequest: 0, navigationEpoch: 0, treeExpanded: new Set(["chats", "tasks", "artifacts"]), artifactRoots: [], artifactEntries: new Map(), artifactExpanded: new Set() };

const el = <K extends keyof HTMLElementTagNameMap>(tag: K, className?: string, text?: string) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};

const object = (value: unknown): Json => value && typeof value === "object" && !Array.isArray(value) ? value as Json : {};
const array = (value: unknown): unknown[] => Array.isArray(value) ? value : [];
const string = (value: unknown): string => typeof value === "string" ? value : "";
const identifier = (value: unknown): string => string(object(value).id) || string(object(value).workspace_id) || string(object(value).channel_id);

async function call(operation: string, args: Json = {}): Promise<Json> {
  const csrf = document.querySelector<HTMLMetaElement>('meta[name="orchard-csrf"]')?.content;
  const response = await fetch("/api/call", {
    method: "POST",
    credentials: "same-origin",
    headers: { "Content-Type": "application/json", ...(csrf ? { "X-Orchard-CSRF": csrf } : {}) },
    body: JSON.stringify({ operation, args }),
  });
  const payload = object(await response.json().catch(() => ({})));
  if (response.status === 401) {
    if (state.poll) window.clearInterval(state.poll);
    renderLogin("Your local session expired after the server restarted. Unlock Orchard again; any unfinished draft remains available.");
    throw new Error("Your session expired. Unlock Orchard again.");
  }
  if (!response.ok) {
    throw new Error(string(payload.error) || `Orchard service returned ${response.status}.`);
  }
  return object(payload.result ?? payload);
}

function notice(message: string, tone: "error" | "info" = "info") {
  const target = document.querySelector<HTMLElement>("#notice");
  if (!target) return;
  target.textContent = message;
  target.dataset.tone = tone;
}

function button(label: string, onClick: () => void | Promise<void>, className = "") {
  const node = el("button", className, label);
  node.type = "button";
  node.addEventListener("click", () => void onClick());
  return node;
}

/** The only renderer for copyable block code.  Keep its source as text, never HTML. */
function codeBlock(content: string, label = "Copy") {
  const block = el("section", "code-block");
  const pre = el("pre", "connection-value");
  const code = el("code");
  code.textContent = content;
  pre.append(code);
  const copy = button(label, async () => {
    try {
      await navigator.clipboard.writeText(code.textContent || "");
      notice("Copied.");
    } catch { notice("Copying is unavailable in this window.", "error"); }
  }, "copy-button subtle");
  block.append(pre, copy);
  return block;
}

function messageBody(body: string) {
  const fragment = document.createDocumentFragment();
  const lines = body.split("\n"); let prose: string[] = []; let code: string[] | undefined; let opener = "";
  const flushProse = () => { if (prose.length) fragment.append(el("p", "", prose.join("\n"))); prose = []; };
  for (const line of lines) {
    if (!code && /^```(?:[A-Za-z0-9_+.-]+)?[ \t]*$/.test(line)) { flushProse(); opener = line; code = []; continue; }
    if (code && /^```[ \t]*$/.test(line)) { fragment.append(codeBlock(code.join("\n"))); code = undefined; continue; }
    if (code) code.push(line); else prose.push(line);
  }
  if (code) prose.push(opener + (code.length ? `\n${code.join("\n")}` : ""));
  flushProse();
  return fragment;
}

function navigate(screen: Screen, detailView?: DetailView, replace = false, url?: string) {
  state.screen = screen;
  state.detailView = detailView;
  state.detailEpoch += 1;
  state.navigationEpoch += 1;
  const route = { screen, detailView, workspaceId: state.workspace?.id, resourceHref: state.activeHref };
  const method = replace ? "replaceState" : "pushState";
  if (url === undefined) history[method](route, ""); else history[method](route, "", url);
}

function workspaceRootHref(workspaceId: string) {
  return canonicalHref({ kind: "broadcast", workspace_id: workspaceId }).replace(/\/broadcast$/, "");
}

function rememberConversationContext() {
  const thread = document.querySelector<HTMLElement>("#thread");
  state.threadScroll = thread?.scrollTop;
  state.composerFocused = document.activeElement?.getAttribute("aria-label") === "Message";
}
function restoreConversationContext() {
  requestAnimationFrame(() => {
    const thread = document.querySelector<HTMLElement>("#thread");
    if (thread && state.threadScroll !== undefined) thread.scrollTop = state.threadScroll;
    if (state.composerFocused) document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Message"]')?.focus();
  });
}

window.addEventListener("popstate", (event) => {
  const route = object(event.state);
  const workspaceId = string(route.workspaceId);
  const screen = (string(route.screen) as Screen) || "workspace";
  const detailView = string(route.detailView) as DetailView || undefined;
  const resourceHref = string(route.resourceHref);
  if (workspaceId && workspaceId !== state.workspace?.id) { void chooseWorkspace(workspaceId, true, false, screen, detailView); return; }
  state.screen = screen;
  state.detailView = detailView;
  state.detailEpoch += 1;
  if (state.screen === "settings") renderSettings(true);
  else if (state.screen === "new-workspace") renderEmptyWorkspace(true);
  else if (state.screen === "home") renderCalmHome(true);
  else if (state.workspace) {
    renderWorkspace();
    if (resourceHref) {
      const local = state.tabs.find((tab) => tab.href === resourceHref);
      if (local) void activateTab(local, true);
      else { const ref = parseHref(resourceHref, state.workspace.id); if (ref) void openResource(descriptor(ref, "Resource"), true); }
    } else renderEmptyViewer();
  }
  else renderCalmHome(true);
});

window.addEventListener("keydown", (event) => {
  if ((event.metaKey || event.ctrlKey) && event.key === "ArrowRight") { event.preventDefault(); cycleTab(1); return; }
  if ((event.metaKey || event.ctrlKey) && event.key === "ArrowLeft") { event.preventDefault(); cycleTab(-1); return; }
  if (event.key !== "Escape") return;
  if (state.screen === "settings") { navigate("workspace"); renderWorkspace(); return; }
  if (state.screen === "new-workspace") { if (state.workspace) { navigate("workspace"); renderWorkspace(); } else renderCalmHome(); return; }
  if (state.detailView === "form") { const target = state.formReturn; state.formReturn = undefined; state.detailView = undefined; if (target) void activateTab(target, true); else renderEmptyViewer(); return; }
});

function cycleTab(direction: number) {
  if (!state.tabs.length) return; const current = state.tabs.findIndex((tab) => tab.href === state.activeHref);
  const next = state.tabs[(current + direction + state.tabs.length) % state.tabs.length]; void activateTab(next);
}

function isDescriptor(tab: AppTab): tab is Descriptor { return tab.kind !== "collection"; }

function collectionTab(collection: CollectionTab["collection"], workspaceId: string): CollectionTab {
  return { kind: "collection", collection, workspaceId, href: `/w/${encodeURIComponent(workspaceId)}/~${collection}`, title: collection === "tasks" ? "Tasks" : collection === "agents" ? "Agents" : "All direct messages" };
}

async function activateTab(tab: AppTab, fromHistory = false) {
  if (isDescriptor(tab)) return openResource(tab, fromHistory);
  const existing = state.tabs.find((item) => item.href === tab.href);
  if (!existing) state.tabs.push(tab);
  state.activeHref = tab.href;
  state.activeResource = undefined;
  state.resourceData = undefined;
  state.resourceLinks = undefined;
  state.navigationEpoch += 1;
  patchTabs();
  if (!fromHistory) history.pushState({ screen: "workspace", workspaceId: tab.workspaceId, resourceHref: tab.href }, "", tab.href);
  if (tab.collection === "tasks") renderTaskCollection();
  else if (tab.collection === "agents") renderAgentCollection();
  else {
    state.selectedConversation = "__all_direct__"; state.conversationKind = "direct"; state.replyTo = undefined; state.conversationMessages = [];
    patchConversations(); patchConversation(); await loadHistory("__all_direct__");
  }
}

function safeLink(value: unknown): HTMLAnchorElement | HTMLSpanElement {
  const label = string(value);
  try {
    const url = new URL(label);
    if (url.protocol === "https:" || url.protocol === "http:") {
      const link = el("a") as HTMLAnchorElement;
      link.href = url.href;
      link.target = "_blank";
      link.rel = "noreferrer";
      link.textContent = label;
      return link;
    }
  } catch { /* displayed as plain text below */ }
  return el("span", "reference-text", label);
}

function referenceNode(value: unknown): HTMLElement {
  const reference = object(value);
  const resource = object(reference.resource);
  if (string(reference.type) === "resource" && state.workspace) {
    const ref = resource as ResourceRef;
    if (ref.kind && ref.workspace_id === state.workspace.id) {
      const tab = descriptor(ref, string(reference.label) || string(ref.path) || string(ref.id) || ref.kind);
      const row = el("p", "reference-text"); row.append(button(tab.title, () => void openResource(tab), "subtle")); return row;
    }
    return el("p", "reference-text", "Attached resource is unavailable.");
  }
  const task = object(reference.task_ref);
  const storeId = string(task.store_id) || string(reference.store_id);
  const taskId = string(task.task_id) || string(reference.task_id);
  if (storeId && taskId && state.workspace) {
    const title = `Task ${taskId}`; const row = el("p", "reference-text");
    row.append(button(title, () => void openResource(descriptor({ kind: "task", workspace_id: state.workspace!.id, store_id: storeId, task_id: taskId }, title)), "subtle"));
    return row;
  }
  const commit = string(reference.commit) || string(reference.sha);
  if (commit) return el("p", "reference-text", `Commit ${commit}`);
  const path = string(reference.path) || string(reference.file);
  const rootId = string(reference.root_id);
  if (path && rootId && state.workspace) {
    const row = el("p", "reference-text");
    row.append(button(path, () => void openResource(descriptor({ kind: "file", workspace_id: state.workspace!.id, root_id: rootId, path, revision: string(reference.revision) || undefined }, path)), "subtle"));
    return row;
  }
  if (path) return el("p", "reference-text", `File ${path}`);
  const label = string(reference.label);
  const url = string(reference.url);
  if (url) {
    const node = safeLink(url);
    const row = el("p", "reference-text");
    if (label) row.append(`${label}: `);
    row.append(node);
    return row;
  }
  return el("p", "reference-text", label || "Reference");
}

function shell(title: string, detail: string) {
  root.replaceChildren();
  const panel = el("section", "welcome");
  const noticeBar = el("p", "notice"); noticeBar.id = "notice"; noticeBar.dataset.tone = "info";
  panel.append(el("p", "eyebrow", "ORCHARD"), el("h1", "", title), el("p", "muted", detail), noticeBar);
  root.append(panel);
}

async function refreshWorkspaces() {
  const result = await call("workspace_list");
  state.workspaces = array(result.workspaces ?? result.items ?? result).map((item) => ({
    id: identifier(item), name: string(object(item).name) || identifier(item), archived: object(item).archived === true,
  })).filter((workspace) => workspace.id && !workspace.archived);
}

async function initialize() {
  try {
    await refreshWorkspaces();
    if (!state.workspaces.length) {
      navigate("new-workspace", undefined, true);
      return renderEmptyWorkspace(true);
    }
    const requestedId = workspaceIdFromLocation();
    const requestedWorkspace = requestedId ? state.workspaces.find((workspace) => workspace.id === requestedId) : undefined;
    if (requestedId && !requestedWorkspace) {
      shell("Resource unavailable", "This link belongs to a workspace that is archived, missing, or unavailable to this session.");
      document.querySelector(".welcome")?.append(button("Open available workspace", () => void chooseWorkspace(state.workspaces[0].id, false, true), "primary"));
      return;
    }
    await chooseWorkspace(requestedWorkspace?.id || state.workspaces[0].id, false, true);
  } catch (error) {
    shell("Orchard is unavailable", "The local workspace service did not respond. Check the connection details in Settings, then try again.");
    const retry = button("Try again", () => void initialize());
    document.querySelector(".welcome")?.append(retry, el("p", "error", message(error)));
  }
}

function workspaceIdFromLocation(): string | undefined {
  const match = /^\/w\/([^/]+)(?:\/|$)/.exec(window.location.pathname);
  if (!match) return undefined;
  try { return decodeURIComponent(match[1]); } catch { return undefined; }
}

async function bootstrap() {
  try {
    const response = await fetch("/api/session", { credentials: "same-origin" });
    const session = object(await response.json().catch(() => ({})));
    if (!response.ok || !session.authenticated) return renderLogin();
    await initialize();
  } catch (error) {
    shell("Orchard is unavailable", "The local Orchard server did not respond.");
    document.querySelector(".welcome")?.append(el("p", "error", message(error)));
  }
}

function renderLogin(reason?: string) {
  shell("Unlock Orchard", reason || "Paste the local access key printed by the Orchard server. It is used only to create this browser session and is never placed in a URL or browser storage.");
  const form = el("form", "stack");
  const token = document.createElement("input"); token.type = "password"; token.autocomplete = "off"; token.placeholder = "Local access key"; token.required = true; token.setAttribute("aria-label", "Local access key");
  const submit = button("Unlock", async () => {
    try {
      const response = await fetch("/api/session", { method: "POST", credentials: "same-origin", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ token: token.value }) });
      const payload = object(await response.json().catch(() => ({})));
      token.value = "";
      if (!response.ok) throw new Error(string(payload.error) || "The access key was not accepted.");
      await initialize();
    } catch (error) { notice(message(error), "error"); }
  }, "primary");
  form.addEventListener("submit", (event) => { event.preventDefault(); submit.click(); }); form.append(token, submit);
  document.querySelector(".welcome")?.append(form);
}

function renderEmptyWorkspace(fromHistory = false) {
  if (!fromHistory) navigate("new-workspace");
  const formEpoch = state.detailEpoch;
  shell("Start a workspace", "Create one workspace, then connect the people, channels and task stores that belong in it.");
  const form = el("form", "stack");
  const name = document.createElement("input");
  name.name = "name";
  name.placeholder = "Workspace name";
  name.setAttribute("aria-label", "Workspace name");
  name.autocomplete = "off";
  name.required = true;
  const submit = button("Create workspace", async () => {
    if (!name.value.trim()) return notice("Give the workspace a name.", "error");
    if (submit.disabled) return;
    submit.disabled = true;
    try {
      const result = await call("workspace_create", { name: name.value.trim() });
      const id = identifier(result.workspace);
      if (!id) throw new Error("The service did not return a workspace id.");
      await refreshWorkspaces();
      if (state.detailEpoch === formEpoch && state.screen === "new-workspace") await chooseWorkspace(id);
    } catch (error) { notice(message(error), "error"); }
    finally { if (document.contains(submit)) submit.disabled = false; }
  }, "primary");
  form.addEventListener("submit", (event) => { event.preventDefault(); submit.click(); });
  const cancel = button("Cancel", () => {
    if (state.workspace) { navigate("workspace"); renderWorkspace(); }
    else renderCalmHome();
  }, "subtle");
  form.append(name, submit, cancel);
  document.querySelector(".welcome")?.append(form);
}

function renderCalmHome(fromHistory = false) {
  if (!fromHistory) navigate("home", undefined, true);
  shell("Orchard", "Create a workspace when you are ready.");
  document.querySelector(".welcome")?.append(button("Create workspace", () => renderEmptyWorkspace(), "primary"));
}

function message(error: unknown) {
  return error instanceof Error ? error.message : String(error || "That action could not be completed.");
}

async function chooseWorkspace(id: string, fromHistory = false, replaceHistory = false, targetScreen: Screen = "workspace", targetDetail?: DetailView) {
  const requestedUrl = window.location.href;
  const workspace = state.workspaces.find((item) => item.id === id);
  if (!workspace) return;
  const retainsDrafts = state.workspace?.id === id;
  const retainedConversation = retainsDrafts ? state.selectedConversation : undefined;
  const retainedKind = state.conversationKind;
  const request = ++state.workspaceRequest;
  const snapshot = await call("workspace_snapshot", { workspace_id: id });
  if (request !== state.workspaceRequest) return;
  state.workspace = workspace;
  state.navigationEpoch += 1;
  state.snapshot = snapshot;
  state.taskBackend = undefined;
  try {
    const settings = await call("settings_get");
    if (request !== state.workspaceRequest) return;
    state.taskBackend = object(object(settings.config).task_backend);
  } catch (error) {
    state.taskBackend = { available: false, error: message(error) };
  }
  state.store = undefined;
  state.selectedTask = undefined;
  state.selectedConversation = retainedConversation;
  state.conversationMessages = [];
  if (!retainsDrafts) {
    state.drafts.clear(); state.tabs = []; state.activeHref = undefined; state.activeResource = undefined; state.resourceData = undefined; state.resourceLinks = undefined;
    state.artifactRoots = []; state.artifactEntries.clear(); state.artifactExpanded.clear();
    state.formReturn = undefined;
  }
  if (!retainsDrafts) {
    state.seenMessageIds = new Set(mailList("history").map((entry) => string(object(entry).id)).filter(Boolean));
    state.unread.clear();
  }
  state.detailView = targetScreen === "workspace" ? targetDetail : undefined;
  state.screen = targetScreen;
  state.senderId = "owner";
  if (targetScreen === "settings") renderSettings(true);
  else if (targetScreen === "new-workspace") renderEmptyWorkspace(true);
  else if (targetScreen === "home") renderCalmHome(true);
  else renderWorkspace();
  const requested = parseHref(requestedUrl, id);
  if (!fromHistory && !requested) navigate("workspace", undefined, replaceHistory, workspaceRootHref(id));
  if (requested) { void openResource(descriptor(requested, "Resource"), true); startPolling(); return; }
  if (state.selectedConversation) {
    await selectConversation(state.conversationKind, state.selectedConversation);
  } else if (mailList("channels").some((item) => identifier(item) === "general")) {
    await selectConversation("channel", "general");
  } else {
    state.conversationKind = retainedKind;
  }
  startPolling();
}

function renderWorkspace() {
  if (!state.workspace) return renderEmptyWorkspace();
  root.replaceChildren();
  const layout = el("div", "workspace-layout");
  layout.id = "workspace-layout";
  const top = el("header", "topbar");
  const select = document.createElement("select");
  select.setAttribute("aria-label", "Workspace");
  for (const workspace of state.workspaces) {
    const option = document.createElement("option");
    option.value = workspace.id;
    option.textContent = workspace.name;
    option.selected = workspace.id === state.workspace.id;
    select.append(option);
  }
  select.addEventListener("change", () => void chooseWorkspace(select.value));
  state.screen = "workspace";
  top.append(el("strong", "brand", "Orchard"), select, button("New workspace", () => { rememberConversationContext(); renderEmptyWorkspace(); }), button("Settings", () => { rememberConversationContext(); renderSettings(); }));
  const noticeBar = el("p", "notice");
  noticeBar.id = "notice";
  noticeBar.dataset.tone = "info";
  top.append(noticeBar);

  const conversations = el("aside", "sidebar resource-tree");
  conversations.id = "conversations";
  const viewer = el("section", "viewer-shell");
  const tabs = el("nav", "tabstrip"); tabs.id = "tabs"; tabs.setAttribute("aria-label", "Open resources");
  const main = el("section", "conversation");
  main.id = "conversation";
  viewer.append(tabs, main);
  layout.append(conversations, viewer);
  root.append(top, layout);
  patchWorkspace();
  restoreConversationContext();
}

function snapshotList(...keys: string[]) {
  for (const key of keys) {
    const values = array(state.snapshot?.[key]);
    if (values.length) return values;
  }
  return [];
}

function mailSnapshot(): Json { return object(state.snapshot?.mail); }
function mailList(name: string): unknown[] {
  return array(mailSnapshot()[name]);
}
function workspaceStores(): unknown[] { return snapshotList("task_stores"); }
function participantName(id: string): string {
  const item = mailList("participants").map(object).find((participant) => identifier(participant) === id);
  return string(item?.name) || id || "Participant";
}

function patchWorkspace() {
  patchConversations();
  patchTabs();
  const active = state.tabs.find((tab) => tab.href === state.activeHref);
  if (active) void activateTab(active, true);
  else renderEmptyViewer();
}

function patchConversations() {
  const panel = document.querySelector<HTMLElement>("#conversations");
  if (!panel || !state.workspace) return;
  panel.replaceChildren();
  const errors = [...snapshotErrors(), ...taskBackendErrors()];
  if (errors.length) panel.append(el("p", "error", `Unavailable source${errors.length === 1 ? "" : "s"}: ${errors.join("; ")}`));

  const section = (name: "chats" | "tasks" | "artifacts", label: string) => {
    const details = document.createElement("details");
    details.className = "tree-group";
    details.open = state.treeExpanded.has(name);
    details.addEventListener("toggle", () => {
      if (details.open) state.treeExpanded.add(name); else state.treeExpanded.delete(name);
      if (name === "artifacts" && details.open && !state.artifactRoots.length) void loadArtifactRoots();
    });
    details.append(el("summary", "tree-heading", label));
    panel.append(details);
    return details;
  };

  const chats = section("chats", "Chats");
  const channels = mailList("channels").filter((channel) => !isSystemChannel(channel));
  if (!channels.length) chats.append(el("p", "muted", "No channels yet."));
  for (const channel of channels) {
    const item = object(channel);
    const id = identifier(item);
    const label = withUnread(string(item.name) || string(item.title) || id, `channel:${id}`);
    chats.append(button(label, async () => {
      await selectConversation("channel", id);
    }, state.selectedConversation === id ? "selected conversation-button" : "conversation-button"));
  }
  chats.append(button("New channel", showChannelForm, "tree-action subtle"));
  const people = mailList("participants").map(object).filter((person) => identifier(person) !== "owner" && identifier(person) !== "orchard");
  if (people.length) chats.append(el("p", "tree-label", "Direct"));
  for (const person of people) { const id = identifier(person); chats.append(button(withUnread(string(person.name) || id, `direct:${id}`), () => selectConversation("direct", id), state.conversationKind === "direct" && state.selectedConversation === id ? "selected conversation-button" : "conversation-button")); }
  if (people.length) chats.append(button(withUnread("All direct messages", "direct:__all_direct__"), () => selectConversation("direct", "__all_direct__"), state.conversationKind === "direct" && state.selectedConversation === "__all_direct__" ? "selected conversation-button" : "conversation-button"));
  chats.append(button(withUnread("Broadcast", "broadcast:broadcast"), () => selectConversation("broadcast", "broadcast"), state.conversationKind === "broadcast" ? "selected conversation-button" : "conversation-button"));
  chats.append(button("Agents", () => void activateTab(collectionTab("agents", state.workspace!.id)), "tree-action subtle"), button("Connection settings", showAgentForm, "tree-action subtle"));

  const tasks = section("tasks", "Tasks");
  tasks.append(button("All tasks", openTasks, "tree-action subtle"), button("Add project", () => void attachRepository(), "tree-action subtle"));
  for (const value of workspaceStores()) {
    const item = object(value); const store = object(item.store); const storeId = identifier(store) || string(store.store_id);
    const storeName = string(store.name) || basename(string(store.path)) || storeId;
    const group = el("div", "tree-subgroup"); group.append(button(storeName, () => { selectStore(item); void activateTab(collectionTab("tasks", state.workspace!.id)); }, "store-button subtle"));
    for (const value of array(item.tasks)) {
      const task = object(value); const taskId = string(task.task_id) || identifier(task);
      if (taskId) group.append(button(string(task.title) || taskId, () => void openResource(descriptor({ kind: "task", workspace_id: state.workspace!.id, store_id: storeId, task_id: taskId }, string(task.title) || taskId)), "conversation-button task-tree-item"));
    }
    tasks.append(group);
  }

  const artifacts = section("artifacts", "Artifacts");
  artifacts.id = "artifact-tree";
  renderArtifactTree(artifacts);
  if (artifacts.open && !state.artifactRoots.length) void loadArtifactRoots();
}

function patchTabs() {
  const tabstrip = document.querySelector<HTMLElement>("#tabs"); if (!tabstrip) return;
  tabstrip.replaceChildren();
  for (const tab of state.tabs) {
    const tabButton = button(tab.title, () => void activateTab(tab), tab.href === state.activeHref ? "selected resource-tab" : "resource-tab");
    tabButton.setAttribute("role", "tab"); tabButton.setAttribute("aria-selected", String(tab.href === state.activeHref));
    const close = button("×", () => closeTab(tab.href), "tab-close subtle"); close.setAttribute("aria-label", `Close ${tab.title}`);
    const item = el("span", "tab-item"); item.append(tabButton, close); tabstrip.append(item);
  }
}

function descriptor(ref: ResourceRef, title: string): Descriptor { return { ref, href: canonicalHref(ref), title, kind: ref.kind }; }
async function openResource(tab: Descriptor, fromHistory = false) {
  const existing = state.tabs.find((item) => item.href === tab.href);
  if (!existing) state.tabs.push(tab);
  state.activeHref = tab.href; state.activeResource = existing && isDescriptor(existing) ? existing : tab; state.navigationEpoch += 1; patchTabs();
  if (!fromHistory) history.pushState({ screen: "workspace", workspaceId: tab.ref.workspace_id, resourceHref: tab.href }, "", tab.href);
  if (tab.href === `/w/${encodeURIComponent(tab.ref.workspace_id)}/tasks`) { renderTaskCollection(); return; }
  if (tab.ref.kind === "channel") return selectConversation("channel", tab.ref.id || "", true);
  if (tab.ref.kind === "direct") return selectConversation("direct", tab.ref.id || "", true);
  if (tab.ref.kind === "broadcast") return selectConversation("broadcast", "broadcast", true);
  const workspaceId = state.workspace?.id; const request = ++state.resourceRequest;
  if (!workspaceId) return;
  try {
    const result = await call("resource_get", { workspace_id: workspaceId, ref: tab.ref });
    if (request !== state.resourceRequest || state.workspace?.id !== workspaceId || state.activeHref !== tab.href) return;
    state.resourceData = object(result.resource); state.resourceLinks = object(result.links);
    const title = string(state.resourceData.title);
    const openTab = state.tabs.find((item) => item.href === tab.href);
    if (title && openTab) openTab.title = title;
    if (title && state.activeResource?.href === tab.href) state.activeResource.title = title;
    patchTabs(); renderResourceDetail(state.resourceData, state.resourceLinks);
  } catch (error) { if (request === state.resourceRequest && state.workspace?.id === workspaceId && state.activeHref === tab.href) renderResourceError(tab.title, message(error)); }
}
function closeTab(href: string) {
  const index = state.tabs.findIndex((tab) => tab.href === href); if (index < 0) return;
  const previousActive = state.activeHref; const wasActive = previousActive === href;
  state.tabs.splice(index, 1); const next = state.tabs[index] || state.tabs[index - 1];
  if (!wasActive) { state.activeHref = previousActive; patchTabs(); return; }
  state.activeHref = next?.href;
  state.resourceRequest += 1; state.navigationEpoch += 1;
  patchTabs();
  if (next) void activateTab(next);
  else {
    if (state.workspace) navigate("workspace", undefined, false, workspaceRootHref(state.workspace.id));
    renderEmptyViewer();
  }
}
function renderEmptyViewer() { state.activeHref = undefined; state.activeResource = undefined; state.resourceData = undefined; state.resourceLinks = undefined; const panel = document.querySelector<HTMLElement>("#conversation"); if (panel) panel.replaceChildren(el("header", "conversation-title", "Choose a resource"), el("p", "empty-state muted", "Select a chat, task, or artifact from the tree.")); }
function renderResourceError(title: string, error: string) { const panel = document.querySelector<HTMLElement>("#conversation"); if (panel) panel.replaceChildren(el("header", "conversation-title", title), el("p", "error", error)); }
function renderResourceDetail(resource: Json, links: Json) {
  const panel = document.querySelector<HTMLElement>("#conversation"); if (!panel) return;
  panel.replaceChildren(el("header", "conversation-title", string(resource.title) || "Resource"));
  const data = object(resource.data); const content = string(data.text);
  const mime = string(data.mime_type).toLowerCase(); const download = string(data.download_url); const preview = string(data.preview_url);
  if (state.activeResource?.ref.kind === "message") {
    const record = object(data.message); const senderId = string(record.sender_id); const destination = object(record.destination); const meta = el("div", "message-meta resource-message-meta");
    if (senderId && state.workspace) meta.append(button(participantName(senderId), () => void openResource(descriptor({ kind: "agent", workspace_id: state.workspace!.id, id: senderId }, participantName(senderId))), "subtle message-sender"));
    const sentAt = string(record.sent_at) || string(record.created_at) || string(record.timestamp); if (sentAt) meta.append(el("time", "message-time", sentAt));
    panel.append(meta);
    const destinationKind = string(destination.kind); const destinationId = string(destination.id);
    if (state.workspace && destinationKind) {
      const conversationRef: ResourceRef | undefined = destinationKind === "channel" && destinationId ? { kind: "channel", workspace_id: state.workspace.id, id: destinationId } : destinationKind === "broadcast" ? { kind: "broadcast", workspace_id: state.workspace.id } : destinationKind === "direct" ? { kind: "direct", workspace_id: state.workspace.id, id: destinationId === "owner" ? senderId : destinationId } : undefined;
      if (conversationRef) panel.append(button(destinationKind === "channel" ? `# ${destinationId}` : destinationKind === "broadcast" ? "Broadcast" : `Direct · ${participantName(conversationRef.id || "")}`, () => void openResource(descriptor(conversationRef, destinationKind)), "subtle destination-link"));
    }
    panel.append(messageBody(string(record.body) || string(record.content) || string(data.body) || content)); for (const ref of array(record.refs ?? data.refs)) panel.append(referenceNode(ref));
  } else if (state.activeResource?.ref.kind === "agent") {
    const participant = object(data.participant); panel.append(el("p", "muted", string(participant.name) || string(participant.id) || "Agent")); if (string(participant.last_contact_at)) panel.append(el("p", "muted", `Last seen ${string(participant.last_contact_at)}`));
  } else if (state.activeResource?.ref.kind === "task") {
    const task = object(data.task); const activeTaskRef = state.activeResource.ref;
    panel.append(el("p", "task-context muted", [string(task.id) || activeTaskRef.task_id, activeTaskRef.store_id].filter(Boolean).join(" · ")));
    panel.append(el("p", "", string(task.description) || string(data.description) || content || "No task description."));
    const dependencyList = array(data.dependencies); const dependencies: Json[] = []; const dependents: Json[] = [];
    for (const value of dependencyList) {
      const relation = object(value); const issueId = string(relation.issue_id); const dependsOnId = string(relation.depends_on_id); const currentId = activeTaskRef.task_id || string(task.id);
      if (issueId && dependsOnId) (issueId === currentId ? dependencies : dependsOnId === currentId ? dependents : dependencies).push(relation);
      else dependencies.push(relation);
    }
    const appendTaskRelations = (label: string, values: Json[], pick: (value: Json) => string) => {
      if (!values.length) return; const section = el("section", "task-relations"); section.append(el("h3", "", label));
      for (const relation of values) { const taskId = pick(relation) || string(object(relation.task_ref).task_id) || string(relation.task_id) || string(relation.id); if (taskId && activeTaskRef.store_id) section.append(button(taskId, () => void openResource(descriptor({ kind: "task", workspace_id: activeTaskRef.workspace_id, store_id: activeTaskRef.store_id, task_id: taskId }, `Task ${taskId}`)), "subtle")); }
      panel.append(section);
    };
    appendTaskRelations("Dependencies", dependencies, (relation) => string(relation.depends_on_id));
    appendTaskRelations("Dependents", dependents, (relation) => string(relation.issue_id));
    const ref = state.activeResource.ref; if (ref.store_id && ref.task_id && state.workspace) {
      const status = document.createElement("select"); status.setAttribute("aria-label", "Task status"); for (const value of ["open", "in_progress", "blocked", "closed"]) { const option = document.createElement("option"); option.value = value; option.textContent = value; option.selected = value === string(task.status) || value === string(data.status); status.append(option); }
      const active = state.activeResource; const workspaceId = state.workspace.id; const href = active.href; const epoch = state.navigationEpoch;
      panel.append(status, button("Update status", async () => { try { await call("task_update", { workspace_id: workspaceId, store_id: ref.store_id, task_id: ref.task_id, status: status.value, request_id: crypto.randomUUID() }); if (state.workspace?.id === workspaceId && state.activeHref === href && state.navigationEpoch === epoch) void openResource(active, true); } catch (error) { notice(message(error), "error"); } }, "subtle"));
    }
  } else if (state.activeResource?.ref.kind === "file") {
    if (typeof data.text === "string") {
      if (content) panel.append(mime.includes("markdown") || /\.(md|markdown)$/i.test(string(resource.title)) ? markdownBody(content, state.activeResource.ref) : codeBlock(content));
      else panel.append(el("p", "empty-state muted", "This file is empty."));
    } else if (data.binary === true) {
      panel.append(el("p", "muted", `${Number(data.byte_length) || 0} bytes`));
      if (preview && /^image\/(?:png|jpe?g|gif|webp)$/i.test(mime)) { const image = document.createElement("img"); image.src = preview; image.alt = string(resource.title) || "Artifact image"; image.className = "artifact-image"; panel.append(image); }
      else panel.append(el("p", "muted", "This file does not have an inline preview."));
    } else panel.append(el("p", "muted", Number(data.byte_length) > 128 * 1024 ? "This text file is too large to preview here." : "This file does not have an inline preview."));
    if (download) { const link = el("a", "download-link", "Download") as HTMLAnchorElement; link.href = download; link.download = ""; panel.append(link); }
  }
  else if (state.activeResource?.ref.kind === "url") { const value = state.activeResource.ref.url || ""; panel.append(el("p", "muted", value)); const link = safeLink(value); if (link instanceof HTMLAnchorElement) { link.textContent = "Open link"; panel.append(link); } }
  else panel.append(el("p", "muted", "No readable detail is available for this resource."));
  const active = state.activeResource;
  if (active) appendResourceActions(panel, active, links);
  if (active?.ref.kind === "file") void renderFileHistory(panel, active);
}

function appendResourceActions(panel: HTMLElement, active: Descriptor, links: Json) {
  const actions = el("div", "resource-actions");
  actions.append(button("Copy link", async () => { try { await navigator.clipboard.writeText(new URL(active.href, window.location.origin).href); notice("Copied."); } catch { notice("Copying is unavailable in this window.", "error"); } }, "subtle"));
  actions.append(button("Add link", () => showLinkForm(active), "subtle"));
  panel.append(actions);
  const outgoing = array(links.outgoing); const incoming = array(links.incoming);
  if (!outgoing.length && !incoming.length) return;
  const related = el("section", "related-links"); related.append(el("h3", "", "Related"));
  const appendLinks = (label: string, values: unknown[], key: "source" | "target") => { if (!values.length) return; related.append(el("h4", "", label)); for (const value of values) { const item = object(value); const ref = object(item[key]) as ResourceRef; const title = string(item.label) || string(ref.path) || string(ref.id) || string(ref.task_id) || string(ref.url) || ref.kind || "Resource"; if (ref.kind && ref.workspace_id === state.workspace?.id) related.append(button(title, () => void openResource(descriptor(ref, title)), "subtle")); else related.append(el("p", "muted", title)); } };
  appendLinks("Links", outgoing, "target"); appendLinks("Backlinks", incoming, "source");
  panel.append(related);
}

function showLinkForm(source: Descriptor) {
  const panel = document.querySelector<HTMLElement>("#conversation"); if (!panel || !state.workspace) return;
  const form = el("form", "inline-form"); const input = document.createElement("input"); input.placeholder = "Paste an Orchard link or https:// URL"; input.setAttribute("aria-label", "Link resource");
  const workspaceId = state.workspace.id; const href = source.href; const epoch = state.navigationEpoch;
  const submit = button("Add link", async () => { const target = attachmentRef(input.value.trim()); if (!target) return; try { await call("resource_link", { workspace_id: workspaceId, source: source.ref, target, request_id: crypto.randomUUID() }); if (state.workspace?.id === workspaceId && state.activeHref === href && state.navigationEpoch === epoch) void openResource(source, true); } catch (error) { notice(message(error), "error"); } }, "primary");
  form.addEventListener("submit", (event) => { event.preventDefault(); submit.click(); }); form.append(input, submit, button("Cancel", () => form.remove(), "subtle")); panel.append(form); input.focus();
}

async function renderFileHistory(panel: HTMLElement, tab: Descriptor) {
  if (!state.workspace || !tab.ref.root_id) return; const workspaceId = state.workspace.id; const request = state.resourceRequest;
  try { const result = await call("artifact_history", { workspace_id: workspaceId, root_id: tab.ref.root_id, path: tab.ref.path || "" }); if (request !== state.resourceRequest || state.workspace?.id !== workspaceId || state.activeHref !== tab.href || !panel.isConnected) return; const versions = array(result.versions); if (!versions.length) return; const section = document.createElement("details"); section.className = "file-history"; section.append(el("summary", "", tab.ref.revision ? `Versions · pinned ${tab.ref.revision.slice(0, 10)}` : "Versions")); const current = button("Working copy", () => void openResource(descriptor({ ...tab.ref, revision: undefined }, tab.title)), tab.ref.revision ? "subtle" : "selected subtle"); section.append(current); for (const value of versions) { const version = object(value); const revision = string(version.revision); if (!revision) continue; section.append(button(`${revision.slice(0, 10)} ${string(version.summary)}`, () => void openResource(descriptor({ ...tab.ref, revision }, tab.title), false), tab.ref.revision === revision ? "selected subtle" : "subtle")); } panel.append(section); } catch (error) { notice(message(error), "error"); }
}

function markdownBody(source: string, file?: ResourceRef) {
  const section = el("article", "markdown-body"); let code: string[] | undefined;
  const flushCode = () => { if (code) { section.append(codeBlock(code.join("\n"))); code = undefined; } };
  for (const line of source.split("\n")) {
    if (/^```[^`]*$/.test(line)) { if (code) flushCode(); else code = []; continue; }
    if (code) { code.push(line); continue; }
    const heading = /^(#{1,3})\s+(.+)$/.exec(line);
    if (heading) { const node = el(`h${heading[1].length}` as "h1" | "h2" | "h3"); appendMarkdownInline(node, heading[2], file); section.append(node); continue; }
    const list = /^[-*]\s+(.+)$/.exec(line);
    if (list) { let ul = section.lastElementChild; if (!(ul instanceof HTMLUListElement)) { ul = document.createElement("ul"); section.append(ul); } const item = el("li"); appendMarkdownInline(item, list[1], file); ul.append(item); continue; }
    if (line) section.append(markdownInline(line, file));
  }
  flushCode(); return section;
}
function markdownInline(line: string, file?: ResourceRef) {
  const paragraph = el("p"); appendMarkdownInline(paragraph, line, file); return paragraph;
}
function appendMarkdownInline(container: HTMLElement, line: string, file?: ResourceRef) {
  const pattern = /\[([^\]]+)\]\(([^\s)]+)\)|`([^`]+)`|\*\*([^*]+)\*\*/g; let cursor = 0;
  for (const match of line.matchAll(pattern)) { container.append(document.createTextNode(line.slice(cursor, match.index))); if (match[1]) { if (/^https?:\/\//.test(match[2])) { const link = safeLink(match[2]); link.textContent = match[1]; container.append(link); } else { const relative = relativeFileRef(file, match[2]); if (relative) container.append(button(match[1], () => void openResource(descriptor(relative, match[1])), "subtle")); else container.append(document.createTextNode(match[1])); } } else if (match[3]) container.append(el("code", "", match[3])); else container.append(el("strong", "", match[4])); cursor = (match.index || 0) + match[0].length; }
  container.append(document.createTextNode(line.slice(cursor)));
}
function relativeFileRef(file: ResourceRef | undefined, href: string): ResourceRef | undefined {
  if (!file?.root_id || !file.path || !state.workspace || href.startsWith("/") || href.startsWith("//") || href.startsWith("#") || /^[A-Za-z][A-Za-z0-9+.-]*:/.test(href) || href.includes("\\")) return undefined;
  const rawPath = href.split(/[?#]/, 1)[0]; if (!rawPath) return undefined;
  let relativePieces: string[];
  try { relativePieces = rawPath.split("/").map((piece) => decodeURIComponent(piece)); } catch { return undefined; }
  if (relativePieces.some((piece) => piece.includes("/") || piece.includes("\\") || piece.includes("\0"))) return undefined;
  const pieces = [...file.path.split("/").slice(0, -1), ...relativePieces]; const output: string[] = [];
  for (const piece of pieces) { if (!piece || piece === ".") continue; if (piece === "..") { if (!output.length) return undefined; output.pop(); } else output.push(piece); }
  return { kind: "file", workspace_id: state.workspace.id, root_id: file.root_id, path: output.join("/"), revision: file.revision };
}
function artifactKey(rootId: string, path: string) { return `${rootId}:${path}`; }

function renderArtifactTree(section?: HTMLElement) {
  const target = section || document.querySelector<HTMLElement>("#artifact-tree");
  if (!target || !state.workspace) return;
  target.querySelectorAll(":scope > .artifact-root, :scope > .artifact-empty").forEach((node) => node.remove());
  if (!state.artifactRoots.length) {
    target.append(el("p", "artifact-empty muted", "Loading artifact roots…"));
    return;
  }
  for (const rootValue of state.artifactRoots) {
    const root = object(rootValue); const rootId = string(root.id); const label = string(root.name) || rootId;
    const container = el("div", "artifact-root");
    if (root.exists === false) {
      container.append(el("p", "tree-label", label), el("p", "muted", "No artifacts yet. Attach a file in a chat to add one."));
      target.append(container); continue;
    }
    const key = artifactKey(rootId, "");
    const toggle = button(`${state.artifactExpanded.has(key) ? "▾" : "▸"} ${label}`, () => toggleArtifactDirectory(rootId, ""), "subtle artifact-toggle");
    container.append(toggle);
    if (state.artifactExpanded.has(key)) container.append(renderArtifactEntries(rootId, ""));
    target.append(container);
  }
}

function renderArtifactEntries(rootId: string, path: string): HTMLElement {
  const list = el("div", "tree-children"); const entries = state.artifactEntries.get(artifactKey(rootId, path));
  if (!entries) { list.append(el("p", "muted", "Loading…")); return list; }
  if (!entries.length) { list.append(el("p", "muted", "Empty folder")); return list; }
  for (const value of entries) {
    const item = object(value); const itemPath = string(item.path); const title = string(item.name) || itemPath; const type = string(item.kind);
    if (type === "directory") {
      const key = artifactKey(rootId, itemPath); const row = el("div", "artifact-directory");
      row.append(button(`${state.artifactExpanded.has(key) ? "▾" : "▸"} ${title}`, () => toggleArtifactDirectory(rootId, itemPath), "subtle artifact-toggle"));
      if (state.artifactExpanded.has(key)) row.append(renderArtifactEntries(rootId, itemPath));
      list.append(row);
    } else if (type === "file") {
      list.append(button(title, () => void openResource(descriptor({ kind: "file", workspace_id: state.workspace!.id, root_id: rootId, path: itemPath }, title)), "subtle artifact-file"));
    } else {
      list.append(el("p", "artifact-special muted", `${title} · ${type || "special entry"}`));
    }
  }
  return list;
}

async function loadArtifactRoots() {
  if (!state.workspace) return; const workspaceId = state.workspace.id;
  try {
    const result = await call("artifact_roots", { workspace_id: workspaceId });
    if (state.workspace?.id !== workspaceId) return;
    state.artifactRoots = array(result.roots).map(object);
    renderArtifactTree();
  } catch (error) { notice(message(error), "error"); }
}

async function toggleArtifactDirectory(rootId: string, path: string) {
  if (!state.workspace) return; const workspaceId = state.workspace.id; const key = artifactKey(rootId, path);
  if (state.artifactExpanded.has(key)) { state.artifactExpanded.delete(key); renderArtifactTree(); return; }
  state.artifactExpanded.add(key); renderArtifactTree();
  if (state.artifactEntries.has(key)) return;
  await loadArtifactDirectory(workspaceId, rootId, path);
}

async function loadArtifactDirectory(workspaceId: string, rootId: string, path: string) {
  const key = artifactKey(rootId, path);
  try {
    const result = await call("artifact_list", { workspace_id: workspaceId, root_id: rootId, path });
    if (state.workspace?.id !== workspaceId || !state.artifactExpanded.has(key)) return;
    state.artifactEntries.set(key, array(result.entries).map(object));
    renderArtifactTree();
  } catch (error) { notice(message(error), "error"); }
}

function isSystemChannel(value: unknown) {
  const item = object(value);
  const text = `${identifier(item)} ${string(item.name)}`.toLowerCase();
  return text.includes("orchard-system") || text.includes("task-receipt") || text.includes("task-receipts");
}

function withUnread(label: string, key: string) {
  const count = state.unread.get(key) || 0;
  return count ? `${label} (${count})` : label;
}

function conversationKey(entry: Json): string {
  const destination = object(entry.destination);
  const kind = string(destination.kind);
  if (kind === "channel") return `channel:${string(destination.id)}`;
  if (kind === "broadcast") return "broadcast:broadcast";
  if (kind === "direct") {
    const sender = string(entry.sender_id); const recipient = string(destination.id);
    return `direct:${sender === "owner" ? recipient : recipient === "owner" ? sender : "__all_direct__"}`;
  }
  return "";
}

function currentConversationKey() {
  return state.selectedConversation ? `${state.conversationKind}:${state.selectedConversation}` : "";
}

function observeMessages(entries: unknown[]) {
  for (const value of entries) {
    const entry = object(value); const id = string(entry.id); const key = conversationKey(entry);
    if (!id || !key || state.seenMessageIds.has(id)) continue;
    state.seenMessageIds.add(id);
    if (key !== currentConversationKey()) state.unread.set(key, (state.unread.get(key) || 0) + 1);
  }
}

function markCurrentConversationRead(entries: unknown[]) {
  for (const value of entries) { const id = string(object(value).id); if (id) state.seenMessageIds.add(id); }
  const key = currentConversationKey();
  if (key) state.unread.set(key, 0);
  if (state.conversationKind === "direct" && state.selectedConversation !== "__all_direct__") state.unread.set("direct:__all_direct__", 0);
}

function snapshotErrors() {
  return array(state.snapshot?.errors).map((entry) => {
    const item = object(entry);
    return [string(item.source), string(item.error)].filter(Boolean).join(": ") || "Unknown source error";
  });
}

function taskBackendErrors() {
  if (!state.taskBackend || state.taskBackend.available !== false) return [];
  return [`task backend: ${string(state.taskBackend.error) || string(state.taskBackend.reason) || "unavailable"}`];
}

async function selectConversation(kind: ConversationKind, id: string, fromResource = false) {
  if (state.workspace) {
    const tab: AppTab = kind === "direct" && id === "__all_direct__" ? collectionTab("directs", state.workspace.id) : descriptor(kind === "channel" ? { kind: "channel", workspace_id: state.workspace.id, id } : kind === "direct" ? { kind: "direct", workspace_id: state.workspace.id, id } : { kind: "broadcast", workspace_id: state.workspace.id }, kind === "broadcast" ? "Broadcast" : kind === "channel" ? `# ${id}` : participantName(id));
    const existing = state.tabs.find((item) => item.href === tab.href);
    if (existing) existing.title = tab.title; else state.tabs.push(tab);
    state.activeHref = tab.href; state.activeResource = isDescriptor(tab) ? tab : undefined; state.resourceLinks = {}; state.navigationEpoch += 1; patchTabs();
    if (!fromResource) history.pushState({ screen: "workspace", workspaceId: state.workspace.id, resourceHref: tab.href }, "", tab.href);
  }
  state.selectedConversation = id;
  state.conversationKind = kind;
  state.replyTo = undefined;
  state.conversationMessages = [];
  patchConversations();
  patchConversation();
  const active = state.activeResource; const epoch = state.navigationEpoch;
  await Promise.all([loadHistory(id), active && id !== "__all_direct__" ? loadConversationResource(active, epoch) : Promise.resolve()]);
}

async function loadConversationResource(active: Descriptor, epoch: number) {
  const workspaceId = active.ref.workspace_id;
  try {
    const result = await call("resource_get", { workspace_id: workspaceId, ref: active.ref });
    if (state.workspace?.id !== workspaceId || state.activeHref !== active.href || state.navigationEpoch !== epoch) return;
    state.resourceData = object(result.resource); state.resourceLinks = object(result.links); rememberConversationContext(); patchConversation(); restoreConversationContext();
  } catch (error) {
    if (state.workspace?.id === workspaceId && state.activeHref === active.href && state.navigationEpoch === epoch) notice(message(error), "error");
  }
}

function patchConversation() {
  const panel = document.querySelector<HTMLElement>("#conversation");
  if (!panel || !state.workspace) return;
  panel.replaceChildren();
  const title = state.selectedConversation ? (state.conversationKind === "broadcast" ? "Broadcast" : state.conversationKind === "direct" ? state.selectedConversation === "__all_direct__" ? "All direct messages" : `Direct · ${participantName(state.selectedConversation)}` : `# ${state.selectedConversation}`) : "Choose a conversation";
  const heading = el("header", "conversation-title");
  if (state.workspace && state.selectedConversation) {
    const ref: ResourceRef = state.conversationKind === "channel" ? { kind: "channel", workspace_id: state.workspace.id, id: state.selectedConversation } : state.conversationKind === "direct" ? { kind: "direct", workspace_id: state.workspace.id, id: state.selectedConversation } : { kind: "broadcast", workspace_id: state.workspace.id };
    heading.append(button(title, () => void openResource(descriptor(ref, title)), "subtle"));
  } else heading.textContent = title;
  panel.append(heading);
  const thread = el("div", "thread");
  thread.id = "thread";
  const messages = state.selectedConversation ? state.conversationMessages : [];
  if (!state.selectedConversation) thread.append(el("p", "muted", "Channels, direct conversations and broadcasts appear here once they exist."));
  if (state.selectedConversation) renderMessageList(thread, messages, state.selectedConversation === "__all_direct__");
  panel.append(thread);
  if (state.selectedConversation && state.selectedConversation !== "__all_direct__") panel.append(composer());
  if (state.activeResource && ["channel", "direct", "broadcast"].includes(state.activeResource.kind)) appendResourceActions(panel, state.activeResource, state.resourceLinks || {});
}

function patchMessages() {
  const thread = document.querySelector<HTMLElement>("#thread");
  if (!thread || !state.selectedConversation) return;
  const previousScroll = thread.scrollTop;
  const wasAtBottom = thread.scrollHeight - thread.scrollTop - thread.clientHeight < 32;
  thread.replaceChildren();
  renderMessageList(thread, state.conversationMessages, state.selectedConversation === "__all_direct__");
  if (wasAtBottom) thread.scrollTop = thread.scrollHeight;
  else thread.scrollTop = previousScroll;
}

function renderMessageList(thread: HTMLElement, messages: unknown[], showDestination = false) {
  thread.replaceChildren();
  for (const entry of messages) {
    const item = object(entry);
    const article = el("article", "message");
    const sender = participantName(string(item.sender_id));
    const destination = object(item.destination);
    const recipient = string(destination.id);
    const meta = el("div", "message-meta");
    if (state.workspace && string(item.sender_id)) meta.append(button(showDestination && recipient ? `${sender} → ${participantName(recipient)}` : sender, () => void openResource(descriptor({ kind: "agent", workspace_id: state.workspace!.id, id: string(item.sender_id) }, sender)), "subtle message-sender"));
    else meta.append(el("strong", "message-sender", showDestination && recipient ? `${sender} → ${participantName(recipient)}` : sender));
    const sentAt = string(item.sent_at) || string(item.created_at) || string(item.timestamp);
    if (sentAt) {
      const date = new Date(sentAt);
      const label = Number.isNaN(date.valueOf()) ? sentAt : date.toLocaleString([], { dateStyle: "short", timeStyle: "short" });
      if (state.workspace && string(item.id)) meta.append(button(label, () => void openResource(descriptor({ kind: "message", workspace_id: state.workspace!.id, id: string(item.id) }, "Message")), "subtle message-time"));
      else meta.append(el("time", "message-time", label));
    }
    article.append(meta);
    article.append(messageBody(string(item.body) || string(item.content)));
    for (const ref of array(item.refs)) article.append(referenceNode(ref));
    if (string(item.thread_id)) article.append(el("p", "muted", "In reply to an earlier message"));
    if (string(item.id) && state.selectedConversation !== "__all_direct__") article.append(button("Reply", () => { state.replyTo = string(item.id); patchConversation(); }, "subtle reply-button"));
    thread.append(article);
  }
  if (!messages.length) thread.append(el("p", "muted", "No messages yet."));
}

function draftKey() { return `${state.conversationKind}:${state.selectedConversation || ""}`; }

function composer() {
  const form = el("form", "composer");
  const input = document.createElement("textarea");
  const previousDraft = state.drafts.get(draftKey());
  input.value = previousDraft?.body || "";
  input.rows = 3;
  input.placeholder = state.replyTo ? "Write a reply" : "Write a message";
  input.setAttribute("aria-label", "Message");
  const key = draftKey();
  const workspaceId = state.workspace?.id;
  const send = button("Send", async () => {
    if (!input.value.trim() || !state.workspace) return;
    if (send.disabled) return;
    send.disabled = true;
    const kind = state.conversationKind;
    const id = state.selectedConversation;
    const target = kind === "broadcast" ? { kind } : { kind, id };
    if (kind !== "broadcast" && !id) return notice("Choose a conversation first.", "error");
    try {
      const rawAttachment = reference.value.trim(); const attached = attachmentRef(rawAttachment);
      if (rawAttachment && !attached) return;
      const refs = attached ? [{ type: "resource", resource: attached }] : [];
      const body = input.value.trim();
      const draftAtSubmit = { body: input.value, reference: reference.value };
      const replyTo = state.replyTo;
      await call("mail_send", { workspace_id: state.workspace.id, request_id: crypto.randomUUID(), sender_id: "owner", destination: target, body, kind: "message", thread_id: replyTo, refs });
      const currentDraft = state.drafts.get(key);
      if (state.workspace?.id !== workspaceId || draftKey() !== key) return;
      if (currentDraft?.body === draftAtSubmit.body && currentDraft?.reference === draftAtSubmit.reference) {
        input.value = "";
        state.drafts.delete(key);
      }
      if (state.replyTo === replyTo) state.replyTo = undefined;
      if (state.selectedConversation) await loadHistory(state.selectedConversation);
    } catch (error) { notice(message(error), "error"); }
    finally { if (document.contains(send)) send.disabled = false; }
  }, "primary");
  form.addEventListener("submit", (event) => { event.preventDefault(); send.click(); });
  const destination = el("p", "muted", `To ${state.conversationKind === "broadcast" ? "everyone" : state.conversationKind === "direct" ? participantName(state.selectedConversation || "") : `# ${state.selectedConversation}`}`);
  const advanced = document.createElement("details");
  advanced.append(el("summary", "", "Attach"));
  const reference = document.createElement("input"); reference.type = "text"; reference.placeholder = "Paste an Orchard link or https:// URL"; reference.value = previousDraft?.reference || ""; reference.setAttribute("aria-label", "Attachment URL");
  const file = document.createElement("input"); file.type = "file"; file.setAttribute("aria-label", "Upload attachment");
  const upload = button("Upload file", async () => {
    const selected = file.files?.[0]; if (!selected || !state.workspace) return;
    const uploadWorkspaceId = state.workspace.id; const uploadKey = key;
    if (selected.size > 512 * 1024) return notice("Attachments must be 512 KiB or smaller.", "error");
    if (upload.disabled) return; upload.disabled = true;
    try {
      const bytes = new Uint8Array(await selected.arrayBuffer()); let binary = ""; for (const byte of bytes) binary += String.fromCharCode(byte);
      const result = await call("artifact_upload", { workspace_id: uploadWorkspaceId, path: selected.name, content_base64: btoa(binary), request_id: crypto.randomUUID() });
      if (state.workspace?.id !== uploadWorkspaceId || draftKey() !== uploadKey) return;
      const resource = object(result.resource); const ref = object(resource.ref) as ResourceRef; if (!ref.kind) throw new Error("Upload did not return a resource.");
      reference.value = string(resource.href) || canonicalHref(ref); saveDraft(); notice("Attachment uploaded.");
      if (ref.kind === "file" && ref.root_id) {
        const key = artifactKey(ref.root_id, ""); state.artifactEntries.delete(key);
        if (state.artifactExpanded.has(key)) void loadArtifactDirectory(uploadWorkspaceId, ref.root_id, "");
        void loadArtifactRoots();
      }
    } catch (error) { notice(message(error), "error"); } finally { if (document.contains(upload)) upload.disabled = false; }
  }, "subtle");
  advanced.append(reference, file, upload);
  const saveDraft = () => state.drafts.set(key, { body: input.value, reference: reference.value });
  input.addEventListener("input", saveDraft); reference.addEventListener("input", saveDraft);
  form.append(destination, input, advanced, send);
  return form;
}

function attachmentRef(value: string): ResourceRef | undefined {
  if (!value || !state.workspace) return undefined;
  const orchard = parseHref(value, state.workspace.id); if (orchard) return orchard;
  try { const url = new URL(value); if (url.protocol === "https:" || url.protocol === "http:") return { kind: "url", workspace_id: state.workspace.id, url: url.href }; } catch { /* validation below */ }
  notice("Attach an Orchard resource link or an HTTP(S) URL.", "error"); return undefined;
}

function basename(path: string) { return path.split("/").filter(Boolean).at(-1) || ""; }
function taskStoreButton(item: Json, label?: string) {
  const store = object(item.store); const id = identifier(store) || string(store.store_id);
  return button(label || string(store.name) || basename(string(store.path)) || id, () => selectStore(item), `${state.store?.id === id ? "selected " : ""}subtle store-button`);
}
function openTasks() {
  const owned = workspaceStores().map(object).find((item) => string(object(item.store).source) === "owned" || identifier(object(item.store)) === "default");
  if (owned && !state.store) selectStore(owned);
  if (state.workspace) void activateTab(collectionTab("tasks", state.workspace.id));
}
function renderTaskCollection() {
  if (!state.workspace) return;
  const panel = document.querySelector<HTMLElement>("#conversation"); if (!panel) return; panel.replaceChildren(el("header", "conversation-title", "Tasks"));
  panel.append(button("Add project", () => void attachRepository(), "subtle"));
  const stores = workspaceStores().map(object);
  if (!stores.length) { panel.append(el("p", "empty-state muted", "No task stores are connected.")); return; }
  const picker = el("div", "collection-picker");
  for (const item of stores) picker.append(taskStoreButton(item));
  panel.append(picker);
  if (state.store) patchTaskPanel(panel); else panel.append(el("p", "muted", "Choose a task store."));
}

function renderAgentCollection() {
  const panel = document.querySelector<HTMLElement>("#conversation"); if (!panel || !state.workspace) return;
  panel.replaceChildren(el("header", "conversation-title", "Agents"));
  const participants = mailList("participants").map(object).filter((person) => identifier(person) !== "owner" && identifier(person) !== "orchard");
  if (!participants.length) panel.append(el("p", "empty-state muted", "No connected agents yet."));
  for (const participant of participants) {
    const id = identifier(participant); const name = string(participant.name) || id;
    const row = el("article", "agent-card"); row.append(button(name, () => void openResource(descriptor({ kind: "agent", workspace_id: state.workspace!.id, id }, name)), "subtle"));
    if (string(participant.last_contact_at)) row.append(el("p", "muted", `Last seen ${string(participant.last_contact_at)}`));
    panel.append(row);
  }
  panel.append(button("Connection settings", showAgentForm, "subtle"));
}
function selectStore(item: Json) {
  const store = object(item.store); const id = identifier(store) || string(store.store_id);
  if (!id) return;
  state.store = { id, path: string(store.path), name: string(store.name), source: string(store.source) };
  state.selectedTask = undefined;
  state.snapshot = { ...state.snapshot, tasks: array(item.tasks) };
  void loadTasks();
}

async function loadHistory(channelId: string) {
  if (!state.workspace) return;
  const workspaceId = state.workspace.id;
  const kind = state.conversationKind;
  const request = ++state.conversationRequest;
  try {
    const args: Json = { workspace_id: workspaceId, destination_kind: kind, latest: true, limit: 100 };
    if (kind === "channel") args.channel_id = channelId;
    const history = await call("mail_history", args);
    let messages = array(history.messages);
    if (kind === "direct" && channelId !== "__all_direct__") messages = messages.filter((value) => {
      const item = object(value); const destination = string(object(item.destination).id); const sender = string(item.sender_id);
      return (sender === "owner" && destination === channelId) || (sender === channelId && destination === "owner");
    });
    if (request !== state.conversationRequest || state.workspace?.id !== workspaceId || state.conversationKind !== kind || state.selectedConversation !== channelId) return;
    state.conversationMessages = messages;
    markCurrentConversationRead(messages);
    patchConversations();
    patchMessages();
  } catch (error) { notice(message(error), "error"); }
}

function showInlineForm(title: string, fields: Array<[string, string, string]>, submitLabel: string, action: (values: Record<string, string>, stillActive: () => boolean) => Promise<void>) {
  const panel = document.querySelector<HTMLElement>("#conversation"); if (!panel) return;
  const returnTab = state.tabs.find((tab) => tab.href === state.activeHref);
  state.formReturn = returnTab;
  navigate("workspace", "form");
  const formEpoch = state.detailEpoch;
  const returnToViewer = () => { state.formReturn = undefined; state.detailView = undefined; if (returnTab) void activateTab(returnTab, true); else renderEmptyViewer(); };
  panel.replaceChildren(button("Back", returnToViewer, "close-button subtle"), el("h2", "", title));
  const form = el("form", "stack"); const inputs = new Map<string, HTMLInputElement>();
  for (const [name, label, placeholder] of fields) { const input = document.createElement("input"); input.name = name; input.required = true; input.placeholder = placeholder; input.setAttribute("aria-label", label); inputs.set(name, input); form.append(el("label", "", label), input); }
  const submit = button(submitLabel, async () => { const values = Object.fromEntries([...inputs].map(([name, input]) => [name, input.value.trim()])); if (Object.values(values).some((value) => !value)) return notice("Complete each field.", "error"); if (submit.disabled) return; submit.disabled = true; try { const stillActive = () => state.detailEpoch === formEpoch && state.detailView === "form"; await action(values, stillActive); if (stillActive()) returnToViewer(); } finally { if (document.contains(submit)) submit.disabled = false; } }, "primary");
  form.addEventListener("submit", (event) => { event.preventDefault(); submit.click(); }); form.append(submit, button("Cancel", returnToViewer, "subtle")); panel.append(form);
}

function showChannelForm() {
  if (!state.workspace) return;
  showInlineForm("New channel", [["name", "Channel name", "Project updates"]], "Create channel", async ({ name }, stillActive) => {
    try { await call("mail_channel_create", { workspace_id: state.workspace!.id, request_id: crypto.randomUUID(), name }); await refreshSnapshot(); void stillActive; }
    catch (error) { notice(message(error), "error"); }
  });
}

function showAgentForm() {
  renderSettings();
}

async function attachRepository() {
  if (!state.workspace) return;
  showInlineForm("Add project", [["path", "Project path", "/path/to/repository"]], "Add project", async ({ path }, stillActive) => {
    try { await call("repository_attach", { workspace_id: state.workspace!.id, path }); await refreshSnapshot(); void stillActive; }
    catch (error) { notice(message(error), "error"); }
  });
}

async function loadTasks() {
  if (!state.workspace || !state.store) return;
  const workspaceId = state.workspace.id; const storeId = state.store.id; const request = ++state.taskRequest;
  try {
    const tasks = await call("tasks_list", { workspace_id: workspaceId, store_id: storeId });
    if (request !== state.taskRequest || state.workspace?.id !== workspaceId || state.store?.id !== storeId) return;
    const loaded = array(tasks.tasks ?? tasks.items ?? tasks);
    const taskStores = workspaceStores().map((value) => {
      const item = object(value);
      return identifier(object(item.store)) === storeId ? { ...item, tasks: loaded } : value;
    });
    state.snapshot = { ...state.snapshot, task_stores: taskStores };
    patchConversations();
    if (state.activeHref === collectionTab("tasks", workspaceId).href) renderTaskCollection();
  } catch (error) { notice(message(error), "error"); }
}

function patchTaskPanel(panel: HTMLElement) {
  const section = el("section", "task-panel");
  const selectedStore = workspaceStores().map(object).find((item) => identifier(object(item.store)) === state.store?.id);
  const create = button("New task", () => void createTask(), "subtle");
  create.disabled = !selectedStore || selectedStore.tasks === null;
  if (create.disabled) create.title = "This task source is unavailable.";
  section.append(create);
  if (selectedStore && selectedStore.tasks === null) {
    section.append(el("p", "error", "This task store is unavailable. Orchard has not substituted empty task data."));
    panel.append(section);
    return;
  }
  for (const item of array(selectedStore?.tasks)) {
    const task = object(item);
    const id = string(task.task_id) || identifier(task);
    section.append(button(`${id} ${string(task.title)}`, () => { state.selectedTask = id; void openResource(descriptor({ kind: "task", workspace_id: state.workspace!.id, store_id: state.store!.id, task_id: id }, string(task.title) || `Task ${id}`)); }, state.selectedTask === id ? "selected subtle" : "subtle"));
  }
  panel.append(section);
}

async function createTask() {
  if (!state.workspace || !state.store) return;
  const workspaceId = state.workspace.id; const storeId = state.store.id;
  showInlineForm("New task", [["title", "Task title", "Describe the next action"]], "Create task", async ({ title }, stillActive) => {
    try { await call("task_create", { workspace_id: workspaceId, store_id: storeId, title, request_id: crypto.randomUUID() }); if (stillActive() && state.workspace?.id === workspaceId && state.store?.id === storeId) await loadTasks(); }
    catch (error) { notice(message(error), "error"); }
  });
}

function renderSettings(fromHistory = false) {
  if (!state.workspace) return;
  if (!fromHistory) navigate("settings");
  shell("Connection settings", "Use this connection only to configure another already-running agent. Orchard never launches or wakes an agent.");
  const panel = document.querySelector<HTMLElement>(".welcome");
  const endpoint = codeBlock("Not loaded");
  const token = codeBlock("Not loaded");
  const claudeConfig = codeBlock("Loading configuration…");
  const codexConfig = codeBlock("Loading configuration…");
  const codexToml = codeBlock("Loading configuration…");
  const back = () => button("Back to workspace", () => { navigate("workspace"); renderWorkspace(); }, "subtle");
  const archive = button("Archive workspace", () => {
    const confirmation = el("section", "stack");
    confirmation.append(el("p", "error", "Archive this workspace? Its data remains on disk, but it leaves the active workspace list."), button("Confirm archive workspace", async () => {
      try {
        await call("workspace_archive", { workspace_id: state.workspace!.id });
        await refreshWorkspaces();
        state.workspace = undefined;
        if (state.workspaces.length) await chooseWorkspace(state.workspaces[0].id);
        else renderEmptyWorkspace();
      } catch (error) { notice(message(error), "error"); }
    }, "primary"), button("Cancel archive", () => confirmation.remove(), "subtle"));
    panel?.append(confirmation);
  }, "subtle");
  panel?.append(back(), el("h2", "", "Endpoint"), endpoint, el("h2", "", "Credential"), token, el("p", "muted", "Each workspace gets its own MCP alias. Orchard does not launch or wake agents."), el("h3", "", "Claude Code"), claudeConfig, el("h3", "", "Codex"), codexConfig, el("h3", "", "Codex TOML"), codexToml, el("p", "muted", "For Codex, ORCHARD_TOKEN must exist in the process that launches the harness; exporting it in a terminal does not change an already-running app. After adding config, reconnect or reload MCP as the harness supports. The agent then calls workspace_info, mail_register or mail_resume, polls mail_inbox, and acknowledges received message ids."), button("Rotate credential", async () => {
    try { await call("rotate_token", { workspace_id: state.workspace!.id }); await loadConnection(endpoint, token, claudeConfig, codexConfig, codexToml); notice("Credential rotated. Replace the affected agent configuration, then reconnect it."); }
    catch (error) { notice(message(error), "error"); }
  }, "primary"), archive, back());
  void loadConnection(endpoint, token, claudeConfig, codexConfig, codexToml);
}

async function loadConnection(endpoint: HTMLElement, token: HTMLElement, claudeConfig?: HTMLElement, codexConfig?: HTMLElement, codexToml?: HTMLElement) {
  if (!state.workspace) return;
  try {
    const connection = await call("connection_info", { workspace_id: state.workspace.id });
    const code = (block?: HTMLElement) => block?.querySelector("code");
    const endpointCode = code(endpoint); const tokenCode = code(token);
    if (endpointCode) endpointCode.textContent = string(connection.endpoint) || "Unavailable";
    if (tokenCode) tokenCode.textContent = string(connection.token) || "Unavailable";
    const alias = `orchard-${state.workspace.id.slice(0, 8)}`;
    const url = endpointCode?.textContent || ""; const secret = tokenCode?.textContent || "";
    const claudeCode = code(claudeConfig); const codexCode = code(codexConfig); const tomlCode = code(codexToml);
    if (claudeCode) claudeCode.textContent = `claude mcp add --transport http ${alias} "${url}" --header "Authorization: Bearer ${secret}"`;
    if (codexCode) codexCode.textContent = `export ORCHARD_TOKEN="${secret}"\ncodex mcp add ${alias} --url "${url}" --bearer-token-env-var ORCHARD_TOKEN`;
    if (tomlCode) tomlCode.textContent = `[mcp_servers."${alias}"]\nurl = "${url}"\nhttp_headers = { Authorization = "Bearer ${secret}" }`;
  } catch (error) { notice(message(error), "error"); }
}

async function refreshSnapshot() {
  if (!state.workspace) return;
  const workspaceId = state.workspace.id;
  const snapshot = await call("workspace_snapshot", { workspace_id: workspaceId });
  if (state.workspace?.id !== workspaceId) return;
  state.snapshot = snapshot;
  try {
    const settings = await call("settings_get");
    if (state.workspace?.id !== workspaceId) return;
    state.taskBackend = object(object(settings.config).task_backend);
  } catch (error) {
    state.taskBackend = { available: false, error: message(error) };
  }
  observeMessages(mailList("history"));
  patchConversations();
}

function startPolling() {
  if (state.poll) window.clearInterval(state.poll);
  state.poll = window.setInterval(() => void refreshSnapshot().catch((error) => notice(message(error), "error")), 8000);
}

void bootstrap();
