// `window.__TAURI__` comes from `withGlobalTauri` in tauri.conf.json (no bundler needed).
const { invoke } = window.__TAURI__.core;

const $ = (id) => document.getElementById(id);

// ---- Torrent list ----
// No torrent engine yet: added torrents only appear here, waiting. Each entry:
// { id, name, size (bytes or null), progress (0..1), status, down, up (bytes/s), peers }
// status: "waiting" | "downloading" | "seeding" | "completed" | "paused"
const torrents = [];
let nextId = 1;

const STATUS_LABELS = {
  waiting: "Waiting for engine",
  downloading: "Downloading",
  seeding: "Seeding",
  completed: "Completed",
  paused: "Paused",
};

function formatBytes(bytes) {
  if (bytes == null) return "–";
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
  for (const t of torrents) {
    const tr = document.createElement("tr");
    tr.append(cell(t.name, "name"), cell(formatBytes(t.size), "num"));

    const progressCell = document.createElement("td");
    const bar = document.createElement("progress");
    bar.max = 1;
    bar.value = t.progress;
    bar.setAttribute("aria-label", `${t.name} progress`);
    progressCell.append(bar, ` ${Math.round(t.progress * 100)}%`);
    tr.append(progressCell);

    tr.append(
      cell(STATUS_LABELS[t.status], `status ${t.status}`),
      cell(`${formatBytes(t.down)}/s`, "num"),
      cell(`${formatBytes(t.up)}/s`, "num"),
      cell(String(t.peers), "num"),
    );

    const actions = document.createElement("td");
    const remove = document.createElement("button");
    remove.className = "button text";
    remove.textContent = "Remove";
    remove.setAttribute("aria-label", `Remove ${t.name}`);
    remove.addEventListener("click", () => {
      torrents.splice(torrents.indexOf(t), 1);
      renderTorrents();
    });
    actions.append(remove);
    tr.append(actions);
    rows.append(tr);
  }
  $("empty").hidden = torrents.length > 0;
  $("total-down").textContent = `${formatBytes(torrents.reduce((sum, t) => sum + t.down, 0))}/s`;
  $("total-up").textContent = `${formatBytes(torrents.reduce((sum, t) => sum + t.up, 0))}/s`;
}

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
$("open-settings").addEventListener("click", () => {
  document.querySelector(`input[name="theme"][value="${currentTheme()}"]`).checked = true;
  $("settings-dialog").showModal();
});
for (const radio of document.querySelectorAll('input[name="theme"]')) {
  radio.addEventListener("change", () => setTheme(radio.value)); // applies right away
}

$("add-form").addEventListener("submit", (event) => {
  const magnet = $("magnet").value.trim();
  const file = $("torrent-file").files[0];
  let name;
  if (magnet) {
    if (!magnet.startsWith("magnet:?")) {
      event.preventDefault();
      $("add-error").textContent = "That doesn't look like a magnet link.";
      return;
    }
    // The display name (dn) is optional; fall back to the info hash.
    const params = new URLSearchParams(magnet.slice("magnet:?".length));
    name = params.get("dn") || params.get("xt")?.split(":").pop() || "Unnamed torrent";
  } else if (file) {
    name = file.name.replace(/\.torrent$/i, "");
  } else {
    event.preventDefault();
    $("add-error").textContent = "Enter a magnet link or choose a .torrent file.";
    return;
  }
  // TODO: hand the magnet/file to the torrent engine once it exists.
  torrents.push({ id: nextId++, name, size: null, progress: 0, status: "waiting", down: 0, up: 0, peers: 0 });
  renderTorrents();
});

// ---- Connection (the VPN bridge) ----
function showStatus(status) {
  $("vpn-state").textContent = status.running ? "Connected" : "Disconnected";
  $("vpn-dot").classList.toggle("on", status.running);
  $("socks-addr").textContent = status.running ? status.socksAddr : "–";
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

renderTorrents();
callBridge("bridge_status");
