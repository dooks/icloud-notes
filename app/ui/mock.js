// WP-I: synthetic implementation of the §3.4 commands/events. Loaded only with ?mock=1.
// Add &signedout=1 to start signed out, &offline=1 to make refresh fail offline.
const q = new URLSearchParams(location.search);
const now = Date.now(), H = 36e5, D = 24 * H;
let signedIn = !q.has("signedout");
let lastSync = new Date(now - 2 * 60e3).toISOString();
let syncing = false;
const listeners = {};
const emit = (name, payload) => (listeners[name] || []).forEach((f) => f({ payload }));

const svg = (c) => "data:image/svg+xml," + encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" width="88" height="88"><rect width="88" height="88" fill="${c}"/></svg>`);
const folders = [
  { id: "DefaultFolder-CloudKit", title: "Notes", kind: { system: "default" }, noteCount: 6 },
  { id: "quick-notes", title: "Quick Notes", kind: { system: "quickNotes" }, noteCount: 1 },
  { id: "F-recipes", title: "Recipes", kind: "user", noteCount: 2 },
  { id: "F-work with a very long folder name to test truncation", title: "Work with a very long folder name to test truncation", kind: "user", noteCount: 0 },
  { id: "TrashFolder-CloudKit", title: "Recently Deleted", kind: { system: "trash" }, noteCount: 1 },
];
const n = (id, folder, title, snippet, ago, extra = {}) =>
  ({ id, folder, title, snippet, modified: now - ago, hasAttachments: false, locked: false, thumbnail: null, ...extra });
const notes = [
  n("N1", "DefaultFolder-CloudKit", "Groceries", "Milk, eggs, bread and a very long list of other things to buy this week", 20 * 60e3),
  n("N2", "DefaultFolder-CloudKit", "Trip planning", "Flights, hotel, and the itinerary for the long weekend", 30 * H, { hasAttachments: true, thumbnail: "A1" }),
  n("N3", "DefaultFolder-CloudKit", "Passwords (locked)", "", 3 * D, { locked: true }),
  n("N4", "DefaultFolder-CloudKit", "Meeting notes", "Agenda and action items", 9 * D),
  n("N5", "DefaultFolder-CloudKit", "", "", 40 * D),
  n("N6", "DefaultFolder-CloudKit", "<img src=x onerror=alert(1)> title", "<b>not bold</b> &amp; escaped", 60 * D),
  n("N7", "quick-notes", "Quick thought", "Remember to call back", 2 * H),
  n("N8", "F-recipes", "Pancakes", "Flour, milk, eggs", 5 * D),
  n("N9", "F-recipes", "Soup", "Onions, stock, thyme", 12 * D),
  n("N10", "TrashFolder-CloudKit", "Old draft", "Deleted note", 80 * D),
];

function page(title, dark) {
  const esc = (s) => s.replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[c]);
  const bg = dark ? "#1c1c1e" : "#fff", fg = dark ? "#f2f2f7" : "#1d1d1f";
  return `<!doctype html><html><head><meta charset="utf-8"><base target="_blank"><title>${esc(title)}</title>
<style>body{background:${bg};color:${fg};font:16px/1.5 system-ui,sans-serif;margin:0;padding:20px 24px;max-width:46em}
h1{font-size:1.6em}a{color:${dark ? "#e0b43a" : "#8a6500"}}blockquote{border-left:3px solid #888;margin-left:0;padding-left:12px}
table{border-collapse:collapse}td{border:1px solid #888;padding:4px 8px}</style></head><body>
<h1>${esc(title || "New Note")}</h1><p>This is synthetic mock content (${dark ? "dark" : "light"}).</p>
<ul><li>First item</li><li>Second item<ul><li>Nested</li></ul></li></ul>
<blockquote>A quote.</blockquote>
<p><a href="https://example.com/">A link that should open in the OS browser</a></p>
<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>
${"<p>Filler paragraph to force scrolling inside the frame.</p>".repeat(30)}</body></html>`;
}

const err = (code, message) => Promise.reject({ code, message });
const delay = (ms, v) => new Promise((r) => setTimeout(() => r(v), ms));
const byDate = (l) => [...l].sort((a, b) => b.modified - a.modified);

const commands = {
  status: () => ({ signedIn, syncing, lastSync: signedIn ? lastSync : null, error: null }),
  sign_in: async () => { await delay(600); signedIn = true; },
  sign_out: () => { signedIn = false; },
  refresh: async () => {
    if (!signedIn) return err("auth", "not signed in");
    if (q.has("offline")) { await delay(400); return err("offline", "You appear to be offline"); }
    syncing = true;
    for (let i = 1; i <= 4; i++) { emit("sync-progress", { pages: i, records: i * 50 }); await delay(400); }
    syncing = false; lastSync = new Date().toISOString();
    emit("sync-done", null);
    return { changed: 3 };
  },
  folders: () => folders,
  notes: ({ folderId }) => byDate(notes.filter((x) => x.folder === folderId)),
  search: async ({ q: s }) => {
    await delay(120);
    s = s.toLowerCase();
    return byDate(notes.filter((x) => !x.locked && (x.title + " " + x.snippet).toLowerCase().includes(s)));
  },
  note_html: async ({ noteId, dark }) => {
    await delay(80);
    const x = notes.find((y) => y.id === noteId);
    return x ? { title: x.title, html: page(x.title, dark) } : err("internal", "no such note");
  },
  open_link: () => {},
};

window.__TAURI__ = {
  core: {
    invoke: async (cmd, args) => {
      if (!(cmd in commands)) return err("internal", "unknown command " + cmd);
      return commands[cmd](args);
    },
    convertFileSrc: (path) => svg(path.startsWith("A1") ? "#c9a227" : "#889"),
  },
  event: { listen: async (name, f) => { (listeners[name] ||= []).push(f); return () => {}; } },
};
if (q.has("signedout")) setTimeout(() => emit("auth-required", null), 0);
