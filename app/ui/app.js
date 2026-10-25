// WP-I: three-pane UI over the §3.4 commands (window.__TAURI__.core.invoke).
// Read-only: no edit affordances. Data goes in via textContent; only the iframe gets srcdoc.
if (new URLSearchParams(location.search).has("mock")) await import("./mock.js");

const T = window.__TAURI__;
const invoke = (cmd, args) => T.core.invoke(cmd, args);
const $ = (id) => document.getElementById(id);
const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
};
const darkMq = matchMedia("(prefers-color-scheme: dark)");
const narrowMq = matchMedia("(max-width: 640px)");

const st = { folders: [], folderId: null, notes: [], noteId: null, query: "", signedIn: true, lastSync: null, syncing: false, error: null, progress: "" };
let listSeq = 0, noteSeq = 0, searchTimer = 0;

// ---- helpers ----
function fmtDate(ms) {
  const d = new Date(ms), now = new Date(), day = 864e5;
  const midnight = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  if (ms >= midnight) return d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  if (ms >= midnight - day) return "Yesterday";
  if (ms >= midnight - 6 * day) return d.toLocaleDateString([], { weekday: "long" });
  return d.toLocaleDateString([], { year: "numeric", month: "short", day: "numeric" });
}
function ago(iso) {
  const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
  if (s < 45) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  return new Date(iso).toLocaleDateString();
}
// Attachment images come only from the notes-asset:// protocol, keyed by our own attachment ids.
function assetUrl(id, variant) {
  const path = `${encodeURIComponent(id)}/${variant}`;
  return T.core.convertFileSrc ? T.core.convertFileSrc(path, "notes-asset") : `notes-asset://${path}`;
}
const folderIcon = (f) =>
  f.kind === "user" ? "📁" : ({ default: "🗒️", trash: "🗑️", quickNotes: "⚡" })[f.kind.system] ?? "📁";
async function call(cmd, args) {
  try {
    return await invoke(cmd, args);
  } catch (e) {
    if (e && e.code === "auth") st.signedIn = false;
    else st.error = e && e.code === "offline" ? "Offline" : (e && e.message) || "Something went wrong";
    renderStatus();
    throw e;
  }
}

// ---- status line ----
function renderStatus() {
  let t;
  if (!st.signedIn) t = "Sign-in needed";
  else if (st.syncing) t = st.progress || "Syncing…";
  else if (st.error) t = st.error + (st.lastSync ? ` · last updated ${ago(st.lastSync)}` : "");
  else t = st.lastSync ? `Updated ${ago(st.lastSync)}` : "Not synced yet";
  $("status-text").textContent = t;
  $("signin").hidden = st.signedIn;
  $("refresh").disabled = !st.signedIn || st.syncing;
  $("refresh").classList.toggle("busy", st.syncing);
}
async function loadStatus() {
  try {
    const s = await invoke("status");
    Object.assign(st, { signedIn: s.signedIn, syncing: s.syncing, lastSync: s.lastSync, error: s.error });
  } catch (e) {
    st.error = (e && e.message) || "Status unavailable";
  }
  renderStatus();
}

// ---- folders ----
function renderFolders() {
  const ul = $("folders");
  ul.replaceChildren();
  for (const f of st.folders) {
    const li = el("li", "folder");
    li.setAttribute("role", "option");
    li.setAttribute("aria-selected", String(f.id === st.folderId && !st.query));
    li.append(el("span", "ico", folderIcon(f)), el("span", "name", f.title), el("span", "count", String(f.noteCount)));
    li.addEventListener("click", () => {
      $("search").value = "";
      st.query = "";
      selectFolder(f.id);
      setView("notes");
    });
    ul.append(li);
  }
}
async function loadFolders() {
  try { st.folders = await call("folders"); } catch { return; }
  if (!st.folders.some((f) => f.id === st.folderId)) st.folderId = st.folders[0]?.id ?? null;
  renderFolders();
}
async function selectFolder(id) {
  st.folderId = id;
  renderFolders();
  await loadNotes();
}

// ---- notes ----
async function loadNotes() {
  const seq = ++listSeq;
  let notes;
  try {
    notes = st.query ? await call("search", { q: st.query })
      : st.folderId ? await call("notes", { folderId: st.folderId }) : [];
  } catch { return; }
  if (seq !== listSeq) return; // a newer request superseded this one
  st.notes = notes;
  if (!notes.some((n) => n.id === st.noteId)) { st.noteId = null; showNote(null); }
  renderNotes();
}
function renderNotes() {
  const ul = $("notes");
  ul.replaceChildren();
  const f = st.folders.find((x) => x.id === st.folderId);
  $("notes-heading").textContent = st.query ? `Search: ${st.query}` : f ? f.title : "";
  const empty = $("notes-empty");
  empty.hidden = st.notes.length > 0;
  empty.textContent = st.query ? "No matching notes" : "No notes";
  for (const n of st.notes) {
    const li = el("li", "note");
    li.setAttribute("role", "option");
    li.setAttribute("aria-selected", String(n.id === st.noteId));
    li.dataset.id = n.id;
    const text = el("div", "text");
    text.append(el("div", "title", (n.locked ? "🔒 " : "") + (n.title || "New Note")));
    const meta = el("div", "meta");
    meta.append(el("span", "when", fmtDate(n.modified)),
      el("span", "snippet", n.locked ? "Locked" : n.snippet || "No additional text"));
    text.append(meta);
    li.append(text);
    if (n.thumbnail && !n.locked) {
      const img = el("img", "thumb");
      img.alt = "";
      img.loading = "lazy";
      img.src = assetUrl(n.thumbnail, "thumb");
      img.addEventListener("error", () => img.remove());
      li.append(img);
    }
    li.addEventListener("click", () => { selectNote(n.id); setView("note"); });
    ul.append(li);
  }
}
async function selectNote(id) {
  st.noteId = id;
  for (const li of $("notes").children) li.setAttribute("aria-selected", String(li.dataset.id === id));
  $("notes").querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" });
  await showNote(id);
}
async function showNote(id) {
  const seq = ++noteSeq, frame = $("note-frame"), msg = $("note-empty");
  const message = (t) => { frame.hidden = true; frame.removeAttribute("srcdoc"); msg.hidden = false; msg.textContent = t; };
  if (!id) return message("Select a note");
  if (st.notes.find((x) => x.id === id)?.locked) return message("🔒 This note is locked");
  let r;
  try { r = await call("note_html", { noteId: id, dark: darkMq.matches }); } catch { return message("Couldn't load this note"); }
  if (seq !== noteSeq) return;
  msg.hidden = true;
  frame.hidden = false;
  frame.srcdoc = r.html;
}

// ---- narrow-width navigation ----
function setView(v) {
  $("app").dataset.view = v;
  $("nav-back").hidden = !narrowMq.matches || v === "folders";
}
$("nav-back").addEventListener("click", () => setView($("app").dataset.view === "note" ? "notes" : "folders"));
narrowMq.addEventListener("change", () => setView($("app").dataset.view));
// On narrow screens the folder list is reached from the notes heading.
$("notes-heading").addEventListener("click", () => { if (narrowMq.matches) setView("folders"); });

// ---- toolbar, keyboard ----
$("search").addEventListener("input", () => {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(() => {
    st.query = $("search").value.trim();
    renderFolders();
    loadNotes();
    setView("notes");
  }, 250);
});
$("refresh").addEventListener("click", refresh);
$("signin").addEventListener("click", async () => {
  try {
    await call("sign_in");
    st.signedIn = true;
    renderStatus();
    await refresh();
  } catch { /* status line shows the error */ }
});
async function refresh() {
  if (st.syncing) return;
  st.syncing = true; st.error = null; st.progress = "";
  renderStatus();
  try {
    await call("refresh");
    st.lastSync = new Date().toISOString();
  } catch { /* handled in call() */ }
  st.syncing = false;
  renderStatus();
  await loadFolders();
  await loadNotes();
}
document.addEventListener("keydown", (e) => {
  const search = $("search");
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") {
    e.preventDefault(); search.focus(); search.select(); return;
  }
  if (e.key === "Escape" && document.activeElement === search) {
    search.value = ""; search.dispatchEvent(new Event("input")); search.blur(); return;
  }
  if ((e.key === "ArrowDown" || e.key === "ArrowUp") && st.notes.length) {
    e.preventDefault();
    const i = st.notes.findIndex((n) => n.id === st.noteId);
    const j = i < 0 ? 0 : Math.min(st.notes.length - 1, Math.max(0, i + (e.key === "ArrowDown" ? 1 : -1)));
    if (j !== i) selectNote(st.notes[j].id);
  }
});
darkMq.addEventListener("change", () => { if (st.noteId) showNote(st.noteId); });

// ---- events ----
T.event.listen("sync-progress", (ev) => {
  st.syncing = true;
  st.progress = `Syncing… ${ev.payload.records} records`;
  renderStatus();
});
T.event.listen("sync-done", async () => {
  st.syncing = false; st.error = null; st.lastSync = new Date().toISOString();
  renderStatus();
  await loadFolders();
  await loadNotes();
});
T.event.listen("auth-required", () => { st.signedIn = false; renderStatus(); });
setInterval(renderStatus, 30000);

// ---- start ----
await loadStatus();
if (st.signedIn) {
  await loadFolders();
  if (st.folderId) await selectFolder(st.folderId);
}
setView("notes");
