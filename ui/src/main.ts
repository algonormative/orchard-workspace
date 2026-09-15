import "./style.css";

type Json = Record<string, unknown>;
type Workspace = { id: string; name: string; archived?: boolean };
type Store = { id: string; path?: string };
type ConversationKind = "channel" | "direct" | "broadcast";
type Draft = { body: string; kind: string; reference: string };

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
  detailView?: "tasks" | "ledger" | "people";
  poll?: number;
} = { workspaces: [], conversationKind: "channel", conversationMessages: [], ledgerMessages: [], drafts: new Map(), workspaceRequest: 0, conversationRequest: 0, ledgerRequest: 0, detailsVisible: false, seenMessageIds: new Set(), unread: new Map() };

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
    if (!state.workspaces.length) return renderEmptyWorkspace();
    await chooseWorkspace(state.workspaces[0].id);
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

function renderEmptyWorkspace() {
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
    try {
      const result = await call("workspace_create", { name: name.value.trim() });
      const id = identifier(result.workspace);
      if (!id) throw new Error("The service did not return a workspace id.");
      await refreshWorkspaces();
      await chooseWorkspace(id);
    } catch (error) { notice(message(error), "error"); }
  }, "primary");
  form.addEventListener("submit", (event) => { event.preventDefault(); submit.click(); });
  form.append(name, submit);
  document.querySelector(".welcome")?.append(form);
}

function message(error: unknown) {
  return error instanceof Error ? error.message : String(error || "That action could not be completed.");
}

async function chooseWorkspace(id: string) {
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
  state.detailView = undefined;
  state.detailsVisible = false;
  state.senderId = "owner";
  renderWorkspace();
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
  top.append(el("strong", "brand", "Orchard"), select, button("New workspace", renderEmptyWorkspace), button("Settings", renderSettings));
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
  panel.append(button("People", () => { state.detailView = "people"; patchDetails(); }, "subtle"));
  panel.append(button("Tasks", () => { state.detailView = "tasks"; patchDetails(); }, "subtle"));
  panel.append(button("Ledger", () => { state.detailView = "ledger"; patchDetails(); }, "subtle"));
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
  const wasAtBottom = thread.scrollHeight - thread.scrollTop - thread.clientHeight < 32;
  thread.replaceChildren();
  renderMessageList(thread, state.conversationMessages, state.selectedConversation === "__all_direct__");
  if (wasAtBottom) thread.scrollTop = thread.scrollHeight;
}

function renderMessageList(thread: HTMLElement, messages: unknown[], showDestination = false) {
  thread.replaceChildren();
  for (const entry of messages) {
    const item = object(entry);
    const article = el("article", "message");
    const sender = participantName(string(item.sender_id));
    const destination = object(item.destination);
    const recipient = string(destination.id);
    article.append(el("strong", "", showDestination && recipient ? `${sender} → ${participantName(recipient)}` : sender), el("p", "", string(item.body) || string(item.content)));
    if (string(item.thread_id)) article.append(el("p", "muted", `Reply thread: ${string(item.thread_id)}`));
    if (string(item.id) && state.selectedConversation !== "__all_direct__") article.append(button("Reply", () => { state.replyTo = string(item.id); patchConversation(); }, "subtle"));
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
  const send = button("Send", async () => {
    if (!input.value.trim() || !state.workspace) return;
    const kind = state.conversationKind;
    const id = state.selectedConversation;
    const target = kind === "broadcast" ? { kind } : { kind, id };
    if (kind !== "broadcast" && !id) return notice("Choose a conversation first.", "error");
    try {
      const refs = reference.value.trim() ? [{ url: reference.value.trim(), label: "Evidence" }] : [];
      await call("mail_send", { workspace_id: state.workspace.id, request_id: crypto.randomUUID(), sender_id: "owner", destination: target, body: input.value.trim(), kind: messageKind.value, thread_id: state.replyTo, refs });
      input.value = "";
      state.drafts.delete(draftKey());
      state.replyTo = undefined;
      if (state.selectedConversation) await loadHistory(state.selectedConversation);
    } catch (error) { notice(message(error), "error"); }
  }, "primary");
  form.addEventListener("submit", (event) => { event.preventDefault(); send.click(); });
  const destination = el("p", "muted", `To ${state.conversationKind === "broadcast" ? "everyone" : state.conversationKind === "direct" ? participantName(state.selectedConversation || "") : `# ${state.selectedConversation}`}`);
  const messageKind = document.createElement("select"); messageKind.setAttribute("aria-label", "Message kind"); for (const value of ["message", "decision", "result", "handoff"]) { const option = document.createElement("option"); option.value = value; option.textContent = value; option.selected = value === (previousDraft?.kind || "message"); messageKind.append(option); }
  const advanced = document.createElement("details");
  advanced.append(el("summary", "", "Add an evidence reference"));
  const reference = document.createElement("input"); reference.type = "url"; reference.placeholder = "https://… (optional)"; reference.value = previousDraft?.reference || ""; reference.setAttribute("aria-label", "Evidence reference URL");
  advanced.append(reference);
  const saveDraft = () => state.drafts.set(draftKey(), { body: input.value, kind: messageKind.value, reference: reference.value });
  input.addEventListener("input", saveDraft); messageKind.addEventListener("change", saveDraft); reference.addEventListener("input", saveDraft);
  form.append(destination, messageKind, input, advanced, send);
  return form;
}

function patchDetails() {
  const panel = document.querySelector<HTMLElement>("#details");
  if (!panel || !state.workspace) return;
  panel.replaceChildren();
  setDetailsVisible(Boolean(state.detailView));
  if (!state.detailView) return;
  if (state.detailView === "people") return patchParticipantPanel(panel);
  if (state.detailView === "ledger") return patchLedgerPanel(panel);
  panel.append(el("h2", "", "Tasks"));
  const backendErrors = taskBackendErrors();
  if (backendErrors.length) {
    panel.append(el("p", "error", "Tasks are unavailable: the Beads backend did not start."));
    return;
  }
  panel.append(button("Attach repository", () => void attachRepository(), "subtle"));
  panel.append(button("Attach task store", () => void attachTaskStore(), "subtle"));
  const stores = workspaceStores();
  if (stores.length) panel.append(el("h3", "", "Task stores"));
  for (const item of stores) {
    const store = object(object(item).store);
    const id = identifier(store) || string(store.store_id);
    panel.append(button(string(store.name) || string(store.path) || id, () => {
      state.store = { id, path: string(store.path) };
      state.selectedTask = undefined;
      state.snapshot = { ...state.snapshot, tasks: array(object(item).tasks) };
      void loadTasks();
    }, `${state.store?.id === id ? "selected " : ""}subtle store-button`));
  }
  if (state.store) patchTaskPanel(panel);
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

function showInlineForm(title: string, fields: Array<[string, string, string]>, submitLabel: string, action: (values: Record<string, string>) => Promise<void>) {
  const panel = document.querySelector<HTMLElement>("#details"); if (!panel) return;
  setDetailsVisible(true);
  panel.replaceChildren(el("h2", "", title));
  const form = el("form", "stack"); const inputs = new Map<string, HTMLInputElement>();
  for (const [name, label, placeholder] of fields) { const input = document.createElement("input"); input.name = name; input.required = true; input.placeholder = placeholder; input.setAttribute("aria-label", label); inputs.set(name, input); form.append(el("label", "", label), input); }
  const submit = button(submitLabel, async () => { const values = Object.fromEntries([...inputs].map(([name, input]) => [name, input.value.trim()])); if (Object.values(values).some((value) => !value)) return notice("Complete each field.", "error"); await action(values); }, "primary");
  form.addEventListener("submit", (event) => { event.preventDefault(); submit.click(); }); form.append(submit); panel.append(form);
}

function showChannelForm() {
  if (!state.workspace) return;
  showInlineForm("New channel", [["name", "Channel name", "Project updates"]], "Create channel", async ({ name }) => {
    try { await call("mail_channel_create", { workspace_id: state.workspace!.id, request_id: crypto.randomUUID(), name }); await refreshSnapshot(); state.detailView = undefined; patchDetails(); }
    catch (error) { notice(message(error), "error"); }
  });
}

function showAgentForm() {
  renderSettings();
}

async function attachRepository() {
  if (!state.workspace) return;
  showInlineForm("Attach repository", [["path", "Repository path", "/path/to/repository"]], "Attach repository", async ({ path }) => {
    try { await call("repository_attach", { workspace_id: state.workspace!.id, path }); await refreshSnapshot(); }
    catch (error) { notice(message(error), "error"); }
  });
}

async function attachTaskStore() {
  if (!state.workspace) return;
  showInlineForm("Attach task store", [["path", "Task store path", "/path/to/.beads"]], "Attach task store", async ({ path }) => {
    try { await call("task_store_attach", { workspace_id: state.workspace!.id, path }); await refreshSnapshot(); }
    catch (error) { notice(message(error), "error"); }
  });
}

async function loadTasks() {
  if (!state.workspace || !state.store) return;
  try {
    const tasks = await call("tasks_list", { workspace_id: state.workspace.id, store_id: state.store.id });
    state.snapshot = { ...state.snapshot, tasks: array(tasks.tasks ?? tasks.items ?? tasks) };
    patchDetails();
  } catch (error) { notice(message(error), "error"); }
}

function patchTaskPanel(panel: HTMLElement) {
  const section = el("section", "task-panel");
  section.append(el("h3", "", "Tasks"));
  section.append(button("New task", () => void createTask(), "subtle"));
  const selectedStore = workspaceStores().map(object).find((item) => identifier(object(item.store)) === state.store?.id);
  if (selectedStore && selectedStore.tasks === null) {
    section.append(el("p", "error", "This task store is unavailable. Orchard has not substituted empty task data."));
    panel.append(section);
    return;
  }
  for (const item of snapshotList("tasks")) {
    const task = object(item);
    const id = string(task.task_id) || identifier(task);
    section.append(button(`${id} ${string(task.title)}`, () => { state.selectedTask = id; void showTask(id); }, state.selectedTask === id ? "selected subtle" : "subtle"));
  }
  panel.append(section);
}

async function createTask() {
  if (!state.workspace || !state.store) return;
  showInlineForm("New task", [["title", "Task title", "Describe the next action"]], "Create task", async ({ title }) => {
    try { await call("task_create", { workspace_id: state.workspace!.id, store_id: state.store!.id, title, request_id: crypto.randomUUID() }); await loadTasks(); }
    catch (error) { notice(message(error), "error"); }
  });
}

async function showTask(taskId: string) {
  if (!state.workspace || !state.store) return;
  try {
    const shown = await call("task_show", { workspace_id: state.workspace.id, store_id: state.store.id, task_id: taskId });
    const task = object(shown.task ?? shown);
    const dependencies = await call("task_dependencies", { workspace_id: state.workspace.id, store_id: state.store.id, task_id: taskId });
    const panel = document.querySelector<HTMLElement>("#details");
    if (!panel) return;
    const detail = el("section", "task-detail");
    detail.append(el("h3", "", string(task.title) || taskId), el("p", "", string(task.description)));
    const status = document.createElement("select"); status.setAttribute("aria-label", "Task status");
    for (const value of ["open", "in_progress", "blocked", "closed"]) { const option = document.createElement("option"); option.value = value; option.textContent = value; option.selected = value === string(task.status); status.append(option); }
    detail.append(status, button("Update status", async () => {
      try { await call("task_update", { workspace_id: state.workspace!.id, store_id: state.store!.id, task_id: taskId, status: status.value, request_id: crypto.randomUUID() }); await loadTasks(); }
      catch (error) { notice(message(error), "error"); }
    }, "subtle"));
    const dependencyItems = array(dependencies.dependencies);
    detail.append(el("h4", "", "Dependencies"), dependencyItems.length ? el("p", "muted", dependencyItems.map((item) => string(object(item).task_id) || string(object(item).id) || "task").join(", ")) : el("p", "muted", "No dependencies."));
    detail.append(button("Mark closed", async () => {
      try {
        await call("task_close", { workspace_id: state.workspace!.id, store_id: state.store!.id, task_id: taskId, request_id: crypto.randomUUID() });
        await loadTasks();
      } catch (error) { notice(message(error), "error"); }
    }, "subtle"));
    panel.append(detail);
  } catch (error) { notice(message(error), "error"); }
}

function patchParticipantPanel(panel: HTMLElement) {
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

function patchLedgerPanel(panel: HTMLElement) {
  panel.append(el("h2", "", "Ledger"));
  const filter = document.createElement("input"); filter.placeholder = "Filter message kind"; filter.setAttribute("aria-label", "Filter ledger kinds");
  const entries = el("div", "ledger-entries"); const all = state.ledgerMessages;
  const paint = () => { entries.replaceChildren(); const needle = filter.value.trim().toLowerCase(); for (const value of all) { const item = object(value); if (needle && !string(item.kind).toLowerCase().includes(needle)) continue; const row = el("article", "message"); row.append(el("strong", "", string(item.kind) || "message"), el("p", "", string(item.body))); for (const ref of array(item.refs)) row.append(referenceNode(ref)); entries.append(row); } if (!entries.childElementCount) entries.append(el("p", "muted", "No matching records.")); };
  filter.addEventListener("input", paint); panel.append(el("p", "muted", "Messages and task receipts are immutable records. Filter by kind."), filter, entries); paint();
  void loadLedger();
}

async function loadLedger() {
  if (!state.workspace) return;
  const workspaceId = state.workspace.id;
  const request = ++state.ledgerRequest;
  try {
    const history = await call("mail_history", { workspace_id: workspaceId, latest: true, limit: 200 });
    if (request !== state.ledgerRequest || state.workspace?.id !== workspaceId) return;
    state.ledgerMessages = array(history.messages).filter((entry) => ["decision", "result", "handoff", "task_intent", "task_result", "task_unknown"].includes(string(object(entry).kind)));
    if (state.detailView === "ledger") patchDetails();
  } catch (error) { notice(message(error), "error"); }
}

function renderSettings() {
  if (!state.workspace) return;
  shell("Connection settings", "Use this connection only to configure another already-running agent. Orchard never launches or wakes an agent.");
  const panel = document.querySelector<HTMLElement>(".welcome");
  const endpoint = el("code", "connection-value", "Not loaded");
  const token = el("code", "connection-value", "Not loaded");
  const claudeConfig = el("pre", "connection-value", "Loading configuration…");
  const codexConfig = el("pre", "connection-value", "Loading configuration…");
  const codexToml = el("pre", "connection-value", "Loading configuration…");
  const copyClaude = button("Copy Claude Code command", async () => {
    try {
      await navigator.clipboard.writeText(claudeConfig.textContent || ""); notice("Claude Code command copied. Run it in the agent's own environment, then reload its MCP configuration.");
    }
    catch { notice("Copying is unavailable in this window.", "error"); }
  }, "subtle");
  const copyCodex = button("Copy Codex command", async () => {
    try { await navigator.clipboard.writeText(codexConfig.textContent || ""); notice("Codex command copied. Export the token before launching the harness, then reload its MCP configuration."); }
    catch { notice("Copying is unavailable in this window.", "error"); }
  }, "subtle");
  const copyToml = button("Copy Codex TOML", async () => {
    try { await navigator.clipboard.writeText(codexToml.textContent || ""); notice("Codex TOML copied. Add it to the harness configuration, then reconnect or reload MCP as that harness supports."); }
    catch { notice("Copying is unavailable in this window.", "error"); }
  }, "subtle");
  const back = button("Back to workspace", renderWorkspace, "subtle");
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
    }, "primary"));
    panel?.append(confirmation);
  }, "subtle");
  panel?.append(el("h2", "", "Endpoint"), endpoint, el("h2", "", "Credential"), token, el("p", "muted", "Each workspace gets its own MCP alias. Orchard does not launch or wake agents."), el("h3", "", "Claude Code"), claudeConfig, copyClaude, el("h3", "", "Codex"), codexConfig, copyCodex, el("h3", "", "Codex TOML"), codexToml, copyToml, el("p", "muted", "For Codex, ORCHARD_TOKEN must exist in the process that launches the harness; exporting it in a terminal does not change an already-running app. After adding config, reconnect or reload MCP as the harness supports. The agent then calls workspace_info, mail_register or mail_resume, polls mail_inbox, and acknowledges received message ids."), button("Rotate credential", async () => {
    try { await call("rotate_token", { workspace_id: state.workspace!.id }); await loadConnection(endpoint, token, claudeConfig, codexConfig, codexToml); notice("Credential rotated. Replace the affected agent configuration, then reconnect it."); }
    catch (error) { notice(message(error), "error"); }
  }, "primary"), archive, back);
  void loadConnection(endpoint, token, claudeConfig, codexConfig, codexToml);
}

async function loadConnection(endpoint: HTMLElement, token: HTMLElement, claudeConfig?: HTMLElement, codexConfig?: HTMLElement, codexToml?: HTMLElement) {
  if (!state.workspace) return;
  try {
    const connection = await call("connection_info", { workspace_id: state.workspace.id });
    endpoint.textContent = string(connection.endpoint) || "Unavailable";
    token.textContent = string(connection.token) || "Unavailable";
    const alias = `orchard-${state.workspace.id.slice(0, 8)}`;
    const url = endpoint.textContent; const secret = token.textContent;
    if (claudeConfig) claudeConfig.textContent = `claude mcp add --transport http ${alias} "${url}" --header "Authorization: Bearer ${secret}"`;
    if (codexConfig) codexConfig.textContent = `export ORCHARD_TOKEN="${secret}"\ncodex mcp add ${alias} --url "${url}" --bearer-token-env-var ORCHARD_TOKEN`;
    if (codexToml) codexToml.textContent = `[mcp_servers."${alias}"]\nurl = "${url}"\nhttp_headers = { Authorization = "Bearer ${secret}" }`;
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
  if (state.detailView === "ledger") void loadLedger();
}

function startPolling() {
  if (state.poll) window.clearInterval(state.poll);
  state.poll = window.setInterval(() => void refreshSnapshot().catch((error) => notice(message(error), "error")), 8000);
}

void bootstrap();
