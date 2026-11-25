// `window.__TAURI__` comes from `withGlobalTauri` in tauri.conf.json (no bundler needed).
const { invoke } = window.__TAURI__.core;

const $ = (id) => document.getElementById(id);

// ---- Torrent list ----
// `torrents` comes from the engine (list_torrents, refreshed every second). Each entry:
// { id, name, size (bytes), progress (0..1), status, error, down, up (bytes/s), peers }
// `pending` holds torrents still being added (magnet links wait for metadata from peers).
let torrents = [];
const pending = [];
let nextPendingId = 1;

const STATUS_LABELS = {
  pending: "Fetching metadata…",
  checking: "Checking",
  downloading: "Downloading",
  seeding: "Seeding",
  paused: "Paused",
  error: "Error",
};

function formatBytes(bytes) {
  if (!bytes) return bytes === 0 ? "0 B" : "–";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (bytes >= 1024 && i < units.length - 1) {
    bytes /= 1024;
    i++;
  }
  return `${bytes.toFixed(i ? 1 : 0)} ${units[i]}`;
}

function cell(text, className) {
  const td = document.createElement("td");
  td.textContent = text;
  if (className) td.className = className;
  return td;
}

function renderTorrents() {
  const rows = $("torrent-rows");
  rows.replaceChildren();
  for (const t of [...torrents, ...pending]) {
    const tr = document.createElement("tr");
    tr.append(cell(t.name, "name"), cell(t.size ? formatBytes(t.size) : "–", "num"));

    const progressCell = document.createElement("td");
    const bar = document.createElement("progress");
    bar.max = 1;
    bar.value = t.progress;
    bar.setAttribute("aria-label", `${t.name} progress`);
    progressCell.append(bar, ` ${Math.floor(t.progress * 100)}%`);
    tr.append(progressCell);

    const status = cell(STATUS_LABELS[t.status] ?? t.status, `status ${t.status}`);
    if (t.error) status.title = t.error;
    tr.append(
      status,
      cell(`${formatBytes(t.down)}/s`, "num"),
      cell(`${formatBytes(t.up)}/s`, "num"),
      cell(String(t.peers), "num"),
    );

    const actions = document.createElement("td");
    if (t.status !== "pending") {
      const remove = document.createElement("button");
      remove.className = "button text";
      remove.textContent = "Remove";
      remove.setAttribute("aria-label", `Remove ${t.name} (downloaded files are kept)`);
      remove.addEventListener("click", async () => {
        try {
          await invoke("remove_torrent", { id: t.id });
        } catch (error) {
          notify(`Couldn't remove ${t.name}: ${error}`);
        }
        refreshTorrents();
      });
      actions.append(remove);
    }
    tr.append(actions);
    rows.append(tr);
  }
  $("empty").hidden = torrents.length + pending.length > 0;
  $("total-down").textContent = `${formatBytes(torrents.reduce((sum, t) => sum + t.down, 0))}/s`;
  $("total-up").textContent = `${formatBytes(torrents.reduce((sum, t) => sum + t.up, 0))}/s`;
}

async function refreshTorrents() {
  try {
    torrents = await invoke("list_torrents");
  } catch (error) {
    console.error("list_torrents failed", error);
  }
  renderTorrents();
}

// ---- Snackbar: messages that outlive a dialog, e.g. an add that failed later ----
let snackbarTimer;
function notify(message) {
  $("snackbar").textContent = message;
  $("snackbar").hidden = false;
  clearTimeout(snackbarTimer);
  snackbarTimer = setTimeout(() => ($("snackbar").hidden = true), 10000);
}
$("snackbar").addEventListener("click", () => ($("snackbar").hidden = true));

// ---- Dialogs ----
for (const button of document.querySelectorAll("[data-close]")) {
  button.addEventListener("click", () => button.closest("dialog").close());
}
$("open-add").addEventListener("click", () => {
  $("add-form").reset();
  $("add-error").textContent = "";
  $("add-dialog").showModal();
});
$("open-connection").addEventListener("click", () => $("connection-dialog").showModal());

// ---- Settings ----
let savedProxyPort;
$("open-settings").addEventListener("click", async () => {
  document.querySelector(`input[name="theme"][value="${currentTheme()}"]`).checked = true;
  $("settings-error").textContent = "";
  const settings = await invoke("get_settings");
  savedProxyPort = settings.proxyPort;
  $("proxy-port").value = settings.proxyPort;
  $("proxy-port-active").textContent = settings.activeProxyPort
    ? `In use now: ${settings.activeProxyPort}.`
    : "No port is in use now, so the torrent engine is off.";
  $("settings-dialog").showModal();
});
// "Done": the theme is already applied; save the proxy port if it changed.
$("settings-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const port = Number($("proxy-port").value);
  if (port !== savedProxyPort) {
    try {
      await invoke("set_proxy_port", { port });
      notify(`Proxy port ${port} saved. Restart Clutch to use it.`);
    } catch (error) {
      $("settings-error").textContent = String(error);
      return;
    }
  }
  $("settings-dialog").close();
});
for (const radio of document.querySelectorAll('input[name="theme"]')) {
  radio.addEventListener("change", () => setTheme(radio.value)); // applies right away
}

$("add-form").addEventListener("submit", async (event) => {
  const magnet = $("magnet").value.trim();
  const file = $("torrent-file").files[0];
  let name;
  let args;
  if (magnet) {
    if (!magnet.startsWith("magnet:?")) {
      event.preventDefault();
      $("add-error").textContent = "That doesn't look like a magnet link.";
      return;
    }
    // The display name (dn) is optional; fall back to the info hash.
    const params = new URLSearchParams(magnet.slice("magnet:?".length));
    name = params.get("dn") || params.get("xt")?.split(":").pop() || "Unnamed torrent";
    args = { magnet };
  } else if (file) {
    name = file.name.replace(/\.torrent$/i, "");
    args = { torrentFile: Array.from(new Uint8Array(await file.arrayBuffer())) };
  } else {
    event.preventDefault();
    $("add-error").textContent = "Enter a magnet link or choose a .torrent file.";
    return;
  }

  // The dialog closes now; the torrent shows as pending until the engine has it.
  const entry = { id: `pending-${nextPendingId++}`, name, size: 0, progress: 0, status: "pending", down: 0, up: 0, peers: 0 };
  pending.push(entry);
  renderTorrents();
  try {
    await invoke("add_torrent", args);
  } catch (error) {
    notify(`Couldn't add ${name}: ${error}`);
  }
  pending.splice(pending.indexOf(entry), 1);
  refreshTorrents();
});

// ---- Connection (the VPN bridge) ----
function showStatus(status) {
  $("vpn-state").textContent = status.running ? "Connected" : "Disconnected";
  $("vpn-dot").classList.toggle("on", status.running);
  $("socks-addr").textContent = status.socksAddr ?? "unavailable";
  const samePort = status.publicTcpPort === status.publicUdpPort;
  $("public-port").textContent = !status.publicIp
    ? "–"
    : samePort
      ? `${status.publicIp}:${status.publicTcpPort}`
      : `${status.publicIp}: TCP ${status.publicTcpPort}, UDP ${status.publicUdpPort}`;
  $("start").disabled = status.running;
  $("stop").disabled = !status.running;
  $("config-path").disabled = status.running;
  $("forward-port").disabled = status.running;
}

async function callBridge(command, args) {
  $("connection-error").textContent = "";
  try {
    showStatus(await invoke(command, args));
  } catch (error) {
    $("connection-error").textContent = String(error);
    $("start").disabled = false;
  }
}

$("connection-form").addEventListener("submit", (event) => {
  event.preventDefault();
  $("start").disabled = true; // connecting takes a moment (handshake, port mapping)
  const port = $("forward-port").value;
  callBridge("start_bridge", {
    configPath: $("config-path").value,
    forwardPort: port ? Number(port) : null,
  });
});
$("stop").addEventListener("click", () => callBridge("stop_bridge"));

refreshTorrents();
setInterval(refreshTorrents, 1000);
invoke("bridge_status").then((status) => {
  showStatus(status);
  if (status.proxyError) {
    notify(`The torrent engine is off: ${status.proxyError}. Choose another proxy port in Settings, then restart Clutch.`);
  }
});
