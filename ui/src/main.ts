import "./style.css";

type Json = Record<string, unknown>;
type Workspace = { id: string; name: string; archived?: boolean };
type Store = { id: string; path?: string; name?: string; source?: string };
type ConversationKind = "channel" | "direct" | "broadcast";
type Draft = { body: string; reference: string };
type DetailView = "tasks" | "activity" | "people" | "form" | "task";
type Screen = "workspace" | "settings" | "new-workspace" | "home";

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
  ledgerMessages: unknown[];
  drafts: Map<string, Draft>;
  workspaceRequest: number;
  conversationRequest: number;
  ledgerRequest: number;
  detailsVisible: boolean;
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
} = { workspaces: [], conversationKind: "channel", conversationMessages: [], ledgerMessages: [], drafts: new Map(), workspaceRequest: 0, conversationRequest: 0, ledgerRequest: 0, detailsVisible: false, seenMessageIds: new Set(), unread: new Map(), screen: "workspace", detailEpoch: 0, taskRequest: 0 };

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

function navigate(screen: Screen, detailView?: DetailView, replace = false) {
  state.screen = screen;
  state.detailView = detailView;
  state.detailEpoch += 1;
  const route = { screen, detailView: detailView === "form" || detailView === "task" ? "tasks" : detailView, workspaceId: state.workspace?.id };
  history[replace ? "replaceState" : "pushState"](route, "");
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
  if (workspaceId && workspaceId !== state.workspace?.id) { void chooseWorkspace(workspaceId, true, false, screen, detailView); return; }
  state.screen = screen;
  state.detailView = detailView;
  state.detailEpoch += 1;
  if (state.screen === "settings") renderSettings(true);
  else if (state.screen === "new-workspace") renderEmptyWorkspace(true);
  else if (state.screen === "home") renderCalmHome(true);
  else if (state.workspace) renderWorkspace();
  else renderCalmHome(true);
});

window.addEventListener("keydown", (event) => {
  if (event.key !== "Escape") return;
  if (state.screen === "settings") { navigate("workspace"); renderWorkspace(); return; }
  if (state.screen === "new-workspace") { if (state.workspace) { navigate("workspace"); renderWorkspace(); } else renderCalmHome(); return; }
  if (state.detailView === "form" || state.detailView === "task") { navigate("workspace", "tasks"); patchDetails(); return; }
  if (state.detailView) closeDetails();
});

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
  const task = object(reference.task_ref);
  const storeId = string(task.store_id) || string(reference.store_id);
  const taskId = string(task.task_id) || string(reference.task_id);
  if (storeId && taskId) return el("p", "reference-text", `Task ${storeId}/${taskId}`);
  const commit = string(reference.commit) || string(reference.sha);
  if (commit) return el("p", "reference-text", `Commit ${commit}`);
  const path = string(reference.path) || string(reference.file);
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
    await chooseWorkspace(state.workspaces[0].id, false, true);
  } catch (error) {
    shell("Orchard is unavailable", "The local workspace service did not respond. Check the connection details in Settings, then try again.");
    const retry = button("Try again", () => void initialize());
    document.querySelector(".welcome")?.append(retry, el("p", "error", message(error)));
  }
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
  const workspace = state.workspaces.find((item) => item.id === id);
  if (!workspace) return;
  const retainsDrafts = state.workspace?.id === id;
  const retainedConversation = retainsDrafts ? state.selectedConversation : undefined;
  const retainedKind = state.conversationKind;
  const request = ++state.workspaceRequest;
  const snapshot = await call("workspace_snapshot", { workspace_id: id });
  if (request !== state.workspaceRequest) return;
  state.workspace = workspace;
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
  state.ledgerMessages = [];
  if (!retainsDrafts) state.drafts.clear();
  if (!retainsDrafts) {
    state.seenMessageIds = new Set(mailList("history").map((entry) => string(object(entry).id)).filter(Boolean));
    state.unread.clear();
  }
  state.detailView = targetScreen === "workspace" ? targetDetail : undefined;
  state.screen = targetScreen;
  state.detailsVisible = Boolean(state.detailView);
  state.senderId = "owner";
  if (targetScreen === "settings") renderSettings(true);
  else if (targetScreen === "new-workspace") renderEmptyWorkspace(true);
  else if (targetScreen === "home") renderCalmHome(true);
  else renderWorkspace();
  if (!fromHistory) navigate("workspace", undefined, replaceHistory);
  if (state.selectedConversation) {
    patchConversations();
    patchConversation();
    void loadHistory(state.selectedConversation);
  } else if (mailList("channels").some((item) => identifier(item) === "general")) {
    state.selectedConversation = "general";
    state.conversationKind = "channel";
    patchConversations();
    patchConversation();
    void loadHistory("general");
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

  const conversations = el("aside", "sidebar");
  conversations.id = "conversations";
  const main = el("section", "conversation");
  main.id = "conversation";
  const details = el("aside", "details");
  details.id = "details";
  layout.append(conversations, main, details);
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
  patchConversation();
  patchDetails();
}

function setDetailsVisible(visible: boolean) {
  state.detailsVisible = visible;
  document.querySelector<HTMLElement>("#workspace-layout")?.classList.toggle("has-details", visible);
}

function patchConversations() {
  const panel = document.querySelector<HTMLElement>("#conversations");
  if (!panel || !state.workspace) return;
  panel.replaceChildren(el("h2", "", "Conversations"));
  const errors = [...snapshotErrors(), ...taskBackendErrors()];
  if (errors.length) panel.append(el("p", "error", `Unavailable source${errors.length === 1 ? "" : "s"}: ${errors.join("; ")}`));
  const channels = mailList("channels").filter((channel) => !isSystemChannel(channel));
  if (!channels.length) panel.append(el("p", "muted", "No channels yet."));
  for (const channel of channels) {
    const item = object(channel);
    const id = identifier(item);
    const label = withUnread(string(item.name) || string(item.title) || id, `channel:${id}`);
    panel.append(button(label, async () => {
      await selectConversation("channel", id);
    }, state.selectedConversation === id ? "selected conversation-button" : "conversation-button"));
  }
  panel.append(button("New channel", showChannelForm, "subtle"));
  const people = mailList("participants").map(object).filter((person) => identifier(person) !== "owner" && identifier(person) !== "orchard");
  if (people.length) panel.append(el("h2", "", "Direct"));
  for (const person of people) { const id = identifier(person); panel.append(button(withUnread(string(person.name) || id, `direct:${id}`), () => selectConversation("direct", id), state.conversationKind === "direct" && state.selectedConversation === id ? "selected conversation-button" : "conversation-button")); }
  if (people.length) panel.append(button(withUnread("All direct messages", "direct:__all_direct__"), () => selectConversation("direct", "__all_direct__"), state.conversationKind === "direct" && state.selectedConversation === "__all_direct__" ? "selected conversation-button" : "conversation-button"));
  panel.append(button(withUnread("Broadcast", "broadcast:broadcast"), () => selectConversation("broadcast", "broadcast"), state.conversationKind === "broadcast" ? "selected conversation-button" : "conversation-button"));
  panel.append(el("h2", "", "Workspace"));
  panel.append(button("Connect agent", showAgentForm, "subtle"));
  panel.append(button("People", () => { navigate("workspace", "people"); patchDetails(); }, "subtle"));
  panel.append(button("Tasks", () => openTasks(), "subtle"));
  panel.append(button("Activity", () => { navigate("workspace", "activity"); patchDetails(); void loadLedger(); }, "subtle"));
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

async function selectConversation(kind: ConversationKind, id: string) {
  state.selectedConversation = id;
  state.conversationKind = kind;
  state.replyTo = undefined;
  state.conversationMessages = [];
  patchConversations();
  patchConversation();
  await loadHistory(id);
}

function patchConversation() {
  const panel = document.querySelector<HTMLElement>("#conversation");
  if (!panel || !state.workspace) return;
  panel.replaceChildren();
  const title = state.selectedConversation ? (state.conversationKind === "broadcast" ? "Broadcast" : state.conversationKind === "direct" ? state.selectedConversation === "__all_direct__" ? "All direct messages" : `Direct · ${participantName(state.selectedConversation)}` : `# ${state.selectedConversation}`) : "Choose a conversation";
  panel.append(el("header", "conversation-title", title));
  const thread = el("div", "thread");
  thread.id = "thread";
  const messages = state.selectedConversation ? state.conversationMessages : [];
  if (!state.selectedConversation) thread.append(el("p", "muted", "Channels, direct conversations and broadcasts appear here once they exist."));
  if (state.selectedConversation) renderMessageList(thread, messages, state.selectedConversation === "__all_direct__");
  panel.append(thread);
  if (state.selectedConversation && state.selectedConversation !== "__all_direct__") panel.append(composer());
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
    meta.append(el("strong", "message-sender", showDestination && recipient ? `${sender} → ${participantName(recipient)}` : sender));
    const sentAt = string(item.sent_at) || string(item.created_at) || string(item.timestamp);
    if (sentAt) {
      const date = new Date(sentAt);
      meta.append(el("time", "message-time", Number.isNaN(date.valueOf()) ? sentAt : date.toLocaleString([], { dateStyle: "short", timeStyle: "short" })));
    }
    article.append(meta);
    article.append(messageBody(string(item.body) || string(item.content)));
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
      const refs = reference.value.trim() ? [{ url: reference.value.trim(), label: "Evidence" }] : [];
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
  advanced.append(el("summary", "", "Add an evidence reference"));
  const reference = document.createElement("input"); reference.type = "url"; reference.placeholder = "https://… (optional)"; reference.value = previousDraft?.reference || ""; reference.setAttribute("aria-label", "Evidence reference URL");
  advanced.append(reference);
  const saveDraft = () => state.drafts.set(key, { body: input.value, reference: reference.value });
  input.addEventListener("input", saveDraft); reference.addEventListener("input", saveDraft);
  form.append(destination, input, advanced, send);
  return form;
}

function patchDetails() {
  const panel = document.querySelector<HTMLElement>("#details");
  if (!panel || !state.workspace) return;
  panel.replaceChildren();
  setDetailsVisible(Boolean(state.detailView));
  if (!state.detailView) return;
  if (state.detailView === "people") return patchParticipantPanel(panel);
  if (state.detailView === "activity") return patchActivityPanel(panel);
  if (state.detailView === "form" || state.detailView === "task") return;
  panel.append(button("Close", closeDetails, "close-button subtle"));
  panel.append(el("h2", "", "Tasks"));
  const backendErrors = taskBackendErrors();
  if (backendErrors.length) {
    panel.append(el("p", "error", "Tasks are unavailable: the Beads backend did not start."));
    return;
  }
  panel.append(button("Add project", () => void attachRepository(), "subtle"));
  const stores = workspaceStores();
  const defaultStore = stores.map(object).find((item) => string(object(item.store).source) === "owned" || identifier(object(item.store)) === "default");
  if (defaultStore) {
    panel.append(el("h3", "", "Workspace tasks"));
    const defaultId = identifier(object(defaultStore).store);
    if (state.store?.id === defaultId) patchTaskPanel(panel);
    else panel.append(taskStoreButton(defaultStore, "View tasks"));
  }
  const repositories = array(object(state.snapshot?.workspace).repositories).map(object);
  if (repositories.length) panel.append(el("h3", "", "Projects"));
  for (const repository of repositories) {
    const section = el("section", "project-tasks");
    const name = string(repository.name) || basename(string(repository.path)) || "Project";
    section.append(el("h4", "", name));
    const storeId = string(repository.task_store_id);
    const storeItem = stores.map(object).find((item) => identifier(object(item.store)) === storeId);
    if (storeItem) {
      section.append(taskStoreButton(storeItem, "View tasks"));
      if (state.store?.id === storeId) patchTaskPanel(section);
    }
    else section.append(projectTaskStatus(repository));
    panel.append(section);
  }
  const repositoryStoreIds = new Set(repositories.map((repository) => string(repository.task_store_id)).filter(Boolean));
  const otherStores = stores.map(object).filter((item) => {
    const store = object(object(item).store);
    const id = identifier(store) || string(store.store_id);
    return id !== (defaultStore ? identifier(object(defaultStore).store) : "") && string(store.source) === "external" && !repositoryStoreIds.has(id);
  });
  if (otherStores.length) {
    panel.append(el("h3", "", "Other task sources"));
    for (const item of otherStores) {
      panel.append(taskStoreButton(item));
      const id = identifier(object(item.store)) || string(object(item.store).store_id);
      if (state.store?.id === id) patchTaskPanel(panel);
    }
  }
}

function closeDetails() { navigate("workspace"); state.store = undefined; state.selectedTask = undefined; patchDetails(); }
function basename(path: string) { return path.split("/").filter(Boolean).at(-1) || ""; }
function projectTaskStatus(repository: Json): HTMLElement {
  const status = string(repository.task_status);
  if (status === "unsupported") {
    const detail = document.createElement("details");
    detail.className = "task-source-error";
    detail.append(el("summary", "", "Project tasks unavailable"), el("p", "error", string(repository.task_error) || "This project’s task source is unsupported."));
    return detail;
  }
  if (status === "missing") return el("p", "muted", "Project tasks unavailable.");
  if (status === "none") return el("p", "muted", "No project tasks found.");
  return el("p", "muted", string(repository.task_error) || "Project tasks unavailable.");
}
function taskStoreButton(item: Json, label?: string) {
  const store = object(item.store); const id = identifier(store) || string(store.store_id);
  return button(label || string(store.name) || basename(string(store.path)) || id, () => selectStore(item), `${state.store?.id === id ? "selected " : ""}subtle store-button`);
}
function openTasks() {
  navigate("workspace", "tasks");
  const owned = workspaceStores().map(object).find((item) => string(object(item.store).source) === "owned" || identifier(object(item.store)) === "default");
  if (owned) selectStore(owned); else patchDetails();
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
  const panel = document.querySelector<HTMLElement>("#details"); if (!panel) return;
  navigate("workspace", "form");
  const formEpoch = state.detailEpoch;
  setDetailsVisible(true);
  panel.replaceChildren(button("Back", () => { navigate("workspace", "tasks"); patchDetails(); }, "close-button subtle"), el("h2", "", title));
  const form = el("form", "stack"); const inputs = new Map<string, HTMLInputElement>();
  for (const [name, label, placeholder] of fields) { const input = document.createElement("input"); input.name = name; input.required = true; input.placeholder = placeholder; input.setAttribute("aria-label", label); inputs.set(name, input); form.append(el("label", "", label), input); }
  const submit = button(submitLabel, async () => { const values = Object.fromEntries([...inputs].map(([name, input]) => [name, input.value.trim()])); if (Object.values(values).some((value) => !value)) return notice("Complete each field.", "error"); if (submit.disabled) return; submit.disabled = true; try { await action(values, () => state.detailEpoch === formEpoch && state.detailView === "form"); } finally { if (document.contains(submit)) submit.disabled = false; } }, "primary");
  form.addEventListener("submit", (event) => { event.preventDefault(); submit.click(); }); form.append(submit, button("Cancel", () => { navigate("workspace", "tasks"); patchDetails(); }, "subtle")); panel.append(form);
}

function showChannelForm() {
  if (!state.workspace) return;
  showInlineForm("New channel", [["name", "Channel name", "Project updates"]], "Create channel", async ({ name }, stillActive) => {
    try { await call("mail_channel_create", { workspace_id: state.workspace!.id, request_id: crypto.randomUUID(), name }); await refreshSnapshot(); if (stillActive()) closeDetails(); }
    catch (error) { notice(message(error), "error"); }
  });
}

function showAgentForm() {
  renderSettings();
}

async function attachRepository() {
  if (!state.workspace) return;
  showInlineForm("Add project", [["path", "Project path", "/path/to/repository"]], "Add project", async ({ path }, stillActive) => {
    try { await call("repository_attach", { workspace_id: state.workspace!.id, path }); await refreshSnapshot(); if (stillActive()) { navigate("workspace", "tasks"); patchDetails(); } }
    catch (error) { notice(message(error), "error"); }
  });
}

async function loadTasks() {
  if (!state.workspace || !state.store) return;
  const workspaceId = state.workspace.id; const storeId = state.store.id; const epoch = state.detailEpoch; const request = ++state.taskRequest;
  try {
    const tasks = await call("tasks_list", { workspace_id: workspaceId, store_id: storeId });
    if (request !== state.taskRequest || epoch !== state.detailEpoch || state.workspace?.id !== workspaceId || state.store?.id !== storeId || state.detailView !== "tasks") return;
    const loaded = array(tasks.tasks ?? tasks.items ?? tasks);
    const taskStores = workspaceStores().map((value) => {
      const item = object(value);
      return identifier(object(item.store)) === storeId ? { ...item, tasks: loaded } : value;
    });
    state.snapshot = { ...state.snapshot, task_stores: taskStores };
    patchDetails();
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
    section.append(button(`${id} ${string(task.title)}`, () => { state.selectedTask = id; void showTask(id); }, state.selectedTask === id ? "selected subtle" : "subtle"));
  }
  panel.append(section);
}

async function createTask() {
  if (!state.workspace || !state.store) return;
  const workspaceId = state.workspace.id; const storeId = state.store.id;
  showInlineForm("New task", [["title", "Task title", "Describe the next action"]], "Create task", async ({ title }, stillActive) => {
    try { await call("task_create", { workspace_id: workspaceId, store_id: storeId, title, request_id: crypto.randomUUID() }); if (stillActive() && state.workspace?.id === workspaceId && state.store?.id === storeId) { navigate("workspace", "tasks"); await loadTasks(); } }
    catch (error) { notice(message(error), "error"); }
  });
}

async function showTask(taskId: string) {
  if (!state.workspace || !state.store) return;
  const workspaceId = state.workspace.id; const storeId = state.store.id; const epoch = state.detailEpoch; const request = ++state.taskRequest;
  try {
    const shown = await call("task_show", { workspace_id: workspaceId, store_id: storeId, task_id: taskId });
    const task = object(shown.task ?? shown);
    const dependencies = await call("task_dependencies", { workspace_id: workspaceId, store_id: storeId, task_id: taskId });
    if (request !== state.taskRequest || epoch !== state.detailEpoch || state.workspace?.id !== workspaceId || state.store?.id !== storeId) return;
    const panel = document.querySelector<HTMLElement>("#details");
    if (!panel) return;
    navigate("workspace", "task");
    const actionEpoch = state.detailEpoch;
    state.selectedTask = taskId;
    panel.replaceChildren(button("Back to tasks", () => { navigate("workspace", "tasks"); patchDetails(); }, "close-button subtle"));
    const detail = el("section", "task-detail");
    detail.append(el("h3", "", string(task.title) || taskId), messageBody(string(task.description)));
    const status = document.createElement("select"); status.setAttribute("aria-label", "Task status");
    for (const value of ["open", "in_progress", "blocked", "closed"]) { const option = document.createElement("option"); option.value = value; option.textContent = value; option.selected = value === string(task.status); status.append(option); }
    detail.append(status, button("Update status", async () => {
      try { await call("task_update", { workspace_id: workspaceId, store_id: storeId, task_id: taskId, status: status.value, request_id: crypto.randomUUID() }); if (state.detailEpoch === actionEpoch && state.detailView === "task" && state.workspace?.id === workspaceId && state.store?.id === storeId) { navigate("workspace", "tasks"); await loadTasks(); } }
      catch (error) { notice(message(error), "error"); }
    }, "subtle"));
    const dependencyItems = array(dependencies.dependencies);
    detail.append(el("h4", "", "Dependencies"), dependencyItems.length ? el("p", "muted", dependencyItems.map((item) => string(object(item).task_id) || string(object(item).id) || "task").join(", ")) : el("p", "muted", "No dependencies."));
    detail.append(button("Mark closed", async () => {
      try {
        await call("task_close", { workspace_id: workspaceId, store_id: storeId, task_id: taskId, request_id: crypto.randomUUID() });
        if (state.detailEpoch === actionEpoch && state.detailView === "task" && state.workspace?.id === workspaceId && state.store?.id === storeId) { navigate("workspace", "tasks"); await loadTasks(); }
      } catch (error) { notice(message(error), "error"); }
    }, "subtle"));
    panel.append(detail);
  } catch (error) { notice(message(error), "error"); }
}

function patchParticipantPanel(panel: HTMLElement) {
  panel.append(button("Close", closeDetails, "close-button subtle"));
  const participants = mailList("participants");
  panel.append(el("h2", "", "People"));
  if (!participants.length) { panel.append(el("p", "muted", "No connected agents yet.")); return; }
  const section = el("section", "participants");
  section.append(el("p", "muted", "Agents are already running elsewhere. Connecting one here records its Orchard participant identity."));
  for (const item of participants) {
    const member = object(item);
    const id = identifier(member);
    const name = id === "orchard" ? "Orchard system" : string(member.name) || id;
    section.append(el("p", "participant", `${name} · last seen ${string(member.last_contact_at) || "unknown"}`));
  }
  panel.append(section);
}

function patchActivityPanel(panel: HTMLElement) {
  panel.append(button("Close", closeDetails, "close-button subtle"));
  panel.append(el("h2", "", "Activity"));
  const filter = document.createElement("input"); filter.placeholder = "Search messages and references"; filter.setAttribute("aria-label", "Search activity");
  const entries = el("div", "ledger-entries");
  entries.id = "activity-entries";
  filter.addEventListener("input", patchActivityEntries);
  panel.append(el("p", "muted", "A searchable history of messages, references, and task receipts."), filter, entries);
  patchActivityEntries();
}

function patchActivityEntries() {
  const filter = document.querySelector<HTMLInputElement>('input[aria-label="Search activity"]');
  const entries = document.querySelector<HTMLElement>("#activity-entries");
  if (!filter || !entries) return;
  entries.replaceChildren();
  const needle = filter.value.trim().toLowerCase();
  for (const value of state.ledgerMessages) {
    const item = object(value);
    const searchable = [string(item.body), string(item.content), string(item.kind), ...array(item.refs).map((ref) => JSON.stringify(ref))].join(" ").toLowerCase();
    if (needle && !searchable.includes(needle)) continue;
    const row = el("article", "message");
    row.append(el("strong", "message-sender", participantName(string(item.sender_id))), messageBody(string(item.body) || string(item.content)));
    for (const ref of array(item.refs)) row.append(referenceNode(ref));
    entries.append(row);
  }
  if (!entries.childElementCount) entries.append(el("p", "muted", "No matching activity."));
}

async function loadLedger() {
  if (!state.workspace) return;
  const workspaceId = state.workspace.id;
  const request = ++state.ledgerRequest;
  try {
    const history = await call("mail_history", { workspace_id: workspaceId, latest: true, limit: 200 });
    if (request !== state.ledgerRequest || state.workspace?.id !== workspaceId) return;
    state.ledgerMessages = array(history.messages);
    if (state.detailView === "activity") patchActivityEntries();
  } catch (error) { notice(message(error), "error"); }
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
  if (state.detailView === "tasks") patchDetails();
  if (state.detailView === "activity") void loadLedger();
}

function startPolling() {
  if (state.poll) window.clearInterval(state.poll);
  state.poll = window.setInterval(() => void refreshSnapshot().catch((error) => notice(message(error), "error")), 8000);
}

void bootstrap();
