// meet-transcribe GUI front end.
//
// No framework and no build step: `tauri.conf.json` points `frontendDist` at
// this folder, so `cargo build` alone produces a working exe and the machine
// never needs Node. Everything talks to Rust through `invoke`, and the engine
// pushes state back over `mt://…` events.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const state = {
  settings: null,
  models: [],
  modelsDir: null,
  /** { state: "ready", root } | { state: "missing", message, searched } */
  openvino: null,
  devices: [],
  status: { phase: "idle" },
  sessions: [],
  /** Session currently being recorded into, if any. */
  recordingId: null,
  /** Session shown in the main pane. Null means the "new transcript" screen. */
  viewId: null,
  viewTitle: null,
  /** Summary backing the header, kept live while recording. */
  viewSummary: null,
  logs: [],
  /** SetupStatus from Rust: what the app has downloaded and what it still needs. */
  setup: null,
  /** Newer release found by the updater, or null. */
  update: null,
  /** True from the moment 更新して再起動 is pressed until the process exits. */
  updating: false,
  version: "",
  /** Whether Windows starts the app at sign-in, as last read from the registry. */
  autostart: false,
};

/** Placeholder title shown until the first utterance names the session. */
const RECORDING_TITLE = "録音中…";

const el = (id) => document.getElementById(id);
const ui = {
  sessionList: el("session-list"),
  transcript: el("transcript"),
  emptyState: el("empty-state"),
  title: el("title"),
  submeta: el("submeta"),
  model: el("model"),
  device: el("device"),
  audio: el("audio"),
  phase: el("phase"),
  statusText: el("status-text"),
  meterFill: el("meter-fill"),
  notice: el("notice"),
  toggle: el("toggle"),
  preload: el("preload"),
  logPanel: el("log-panel"),
};

// ------------------------------------------------------------------ helpers

const pad = (n) => String(n).padStart(2, "0");

function clockTime(iso) {
  const d = new Date(iso);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function dayLabel(iso) {
  const d = new Date(iso);
  const today = new Date();
  const yesterday = new Date(today.getTime() - 86400000);
  const same = (a, b) =>
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate();
  if (same(d, today)) return "今日";
  if (same(d, yesterday)) return "昨日";
  return `${d.getFullYear()}/${pad(d.getMonth() + 1)}/${pad(d.getDate())}`;
}

function duration(seconds) {
  const total = Math.round(seconds || 0);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return m > 0 ? `${m}分${pad(s)}秒` : `${s}秒`;
}

function deviceValue(kind, id) {
  return `${kind}|${id ?? ""}`;
}

function parseDeviceValue(value) {
  const at = value.indexOf("|");
  const kind = value.slice(0, at);
  const id = value.slice(at + 1);
  return { kind, id: id === "" ? null : id };
}

function log(level, message) {
  state.logs.push({ level, message, at: new Date() });
  if (state.logs.length > 400) state.logs.splice(0, state.logs.length - 400);
  renderLogs();
}

async function call(command, args) {
  try {
    return await invoke(command, args);
  } catch (error) {
    log("error", String(error));
    showNotice("error", String(error));
    throw error;
  }
}

function showNotice(kind, message) {
  ui.notice.className = `notice ${kind}`;
  ui.notice.textContent = message;
}

function clearNotice() {
  ui.notice.className = "notice hidden";
  ui.notice.textContent = "";
}

/** Bytes as MB, which is the only unit anything here is measured in. */
function mb(bytes) {
  return `${(bytes / (1024 * 1024)).toFixed(0)} MB`;
}

// ------------------------------------------------------------------ sidebar

function renderSessions() {
  ui.sessionList.replaceChildren();
  if (state.sessions.length === 0) {
    const empty = document.createElement("p");
    empty.className = "muted";
    empty.style.padding = "10px 10px 0";
    empty.textContent = "保存された文字起こしはまだありません。";
    ui.sessionList.append(empty);
    return;
  }

  let currentGroup = null;
  for (const session of state.sessions) {
    const group = dayLabel(session.startedAt);
    if (group !== currentGroup) {
      currentGroup = group;
      const heading = document.createElement("div");
      heading.className = "session-group";
      heading.textContent = group;
      ui.sessionList.append(heading);
    }

    const item = document.createElement("button");
    item.className = "session-item";
    if (session.id === state.viewId) item.classList.add("active");
    item.onclick = () => openSession(session.id);

    const name = document.createElement("span");
    name.className = "name";
    if (session.id === state.recordingId) {
      const dot = document.createElement("span");
      dot.className = "dot";
      dot.textContent = "● ";
      name.append(dot);
    }
    name.append(document.createTextNode(session.title));

    const meta = document.createElement("span");
    meta.className = "meta";
    const parts = [clockTime(session.startedAt), `${session.segmentCount} 発話`];
    if (session.interrupted && session.id !== state.recordingId) parts.push("中断");
    meta.textContent = parts.join(" · ");

    item.append(name, meta);
    ui.sessionList.append(item);
  }
}

async function refreshSessions() {
  state.sessions = await call("list_sessions");
  renderSessions();
}

// --------------------------------------------------------------- transcript

function segmentNode(segment) {
  const line = document.createElement("div");
  line.className = "line";

  const stamp = document.createElement("div");
  stamp.className = "stamp";
  stamp.textContent = clockTime(segment.at);

  const body = document.createElement("div");
  body.className = "text";
  body.textContent = segment.text;

  if (state.settings?.showMetrics) {
    const metrics = document.createElement("span");
    metrics.className = "metrics";
    const parts = [`${segment.audioS.toFixed(1)}s`, `RTF ${segment.rtf.toFixed(2)}`];
    // Only worth a word when the utterance actually queued; absent from
    // transcripts recorded before it was tracked.
    if (segment.waitS >= 0.5) parts.push(`待ち ${segment.waitS.toFixed(1)}s`);
    metrics.textContent = parts.join(" · ");
    body.append(metrics);
  }

  line.append(stamp, body);
  return line;
}

function atBottom() {
  const node = ui.transcript;
  return node.scrollHeight - node.scrollTop - node.clientHeight < 80;
}

function appendSegment(segment) {
  const stick = state.settings?.autoScroll !== false && atBottom();
  ui.emptyState?.remove();
  ui.emptyState = null;
  const node = segmentNode(segment);
  node.classList.add("fresh");
  ui.transcript.append(node);
  if (stick) ui.transcript.scrollTop = ui.transcript.scrollHeight;

  // Keep the header honest as the run goes: the sidebar only refreshes on a
  // timer, but the counts above the transcript should never lag what is on
  // screen. The backend names an untitled session after its first utterance;
  // mirror that here so the placeholder does not linger.
  if (state.viewSummary) {
    state.viewSummary.segmentCount += 1;
    state.viewSummary.audioSeconds += segment.audioS;
    if (state.viewSummary.title === RECORDING_TITLE) {
      const head = segment.text.slice(0, 28);
      state.viewSummary.title = segment.text.length > 28 ? `${head}…` : head;
    }
    setHeader(state.viewSummary);
  }
}

function renderTranscript(segments) {
  ui.transcript.replaceChildren();
  if (!segments || segments.length === 0) {
    const empty = document.createElement("div");
    empty.className = "empty";
    const title = document.createElement("p");
    title.className = "empty-title";
    title.textContent = state.viewId
      ? "この文字起こしには発話がありません"
      : "まだ文字起こしはありません";
    const body = document.createElement("p");
    body.className = "empty-body";
    body.textContent = state.viewId
      ? "音声が検出されなかったか、開始直後に停止されています。"
      : "下でモデル・デバイス・音声入力を選び、開始を押してください。";
    empty.append(title, body);
    ui.transcript.append(empty);
    ui.emptyState = empty;
    return;
  }
  for (const segment of segments) ui.transcript.append(segmentNode(segment));
  ui.transcript.scrollTop = ui.transcript.scrollHeight;
}

function setHeader(summary) {
  const hasSession = Boolean(state.viewId);
  ui.title.textContent = summary ? summary.title : "新しい文字起こし";
  state.viewTitle = summary ? summary.title : null;
  state.viewSummary = summary;

  if (summary) {
    const bits = [
      `${new Date(summary.startedAt).toLocaleString("ja-JP")} 開始`,
      `${summary.model} / ${summary.device}`,
      `${summary.audioDevice}`,
      `${summary.segmentCount} 発話 · 音声 ${duration(summary.audioSeconds)}`,
    ];
    if (summary.interrupted && summary.id !== state.recordingId) {
      bits.push("正常終了せずに中断");
    }
    ui.submeta.textContent = bits.join("  ·  ");
  } else {
    ui.submeta.textContent = "開始するとここに保存されます";
  }

  for (const id of ["rename", "copy", "export", "delete"]) {
    el(id).disabled = !hasSession;
  }
  el("delete").disabled = !hasSession || state.viewId === state.recordingId;
}

async function openSession(id) {
  const detail = await call("get_session", { id });
  state.viewId = id;
  setHeader(detail.summary);
  renderTranscript(detail.segments);
  renderSessions();
}

function newSessionView() {
  state.viewId = null;
  setHeader(null);
  renderTranscript([]);
  renderSessions();
}

// -------------------------------------------------------------- device lists

function renderModels() {
  const previous = state.settings?.model;
  ui.model.replaceChildren();

  if (!state.modelsDir) {
    const option = document.createElement("option");
    option.value = "";
    option.textContent = "モデルフォルダ未設定 — 設定から選択";
    ui.model.append(option);
    ui.model.disabled = true;
    return;
  }
  if (state.models.length === 0) {
    const option = document.createElement("option");
    option.value = "";
    option.textContent = "モデルが見つかりません";
    ui.model.append(option);
    ui.model.disabled = true;
    return;
  }

  ui.model.disabled = false;
  for (const model of state.models) {
    const option = document.createElement("option");
    option.value = model.name;
    option.disabled = !model.usable;
    const size = model.sizeMb > 0 ? ` (${model.sizeMb} MB)` : "";
    option.textContent = model.usable
      ? `${model.name}${size}`
      : `${model.name} — 使用不可`;
    ui.model.append(option);
  }

  const usable = state.models.filter((m) => m.usable);
  const wanted =
    state.models.find((m) => m.name === previous && m.usable) ?? usable[0];
  if (wanted) ui.model.value = wanted.name;
}

function renderDevices() {
  const want = state.settings?.audio ?? { kind: "loopback", id: null };
  ui.audio.replaceChildren();

  const groups = [
    { kind: "loopback", label: "ループバック (スピーカーに出ている音)" },
    { kind: "microphone", label: "マイク" },
  ];

  for (const group of groups) {
    const optgroup = document.createElement("optgroup");
    optgroup.label = group.label;

    const auto = document.createElement("option");
    auto.value = deviceValue(group.kind, null);
    auto.textContent = "既定のデバイスを使う";
    optgroup.append(auto);

    for (const device of state.devices.filter((d) => d.kind === group.kind)) {
      const option = document.createElement("option");
      option.value = deviceValue(device.kind, device.id);
      option.textContent = device.isDefault ? `${device.name}（既定）` : device.name;
      optgroup.append(option);
    }
    ui.audio.append(optgroup);
  }

  const wanted = deviceValue(want.kind, want.id);
  ui.audio.value = wanted;
  // The saved endpoint may have been unplugged since last time.
  if (ui.audio.value !== wanted) {
    ui.audio.value = deviceValue(want.kind, null);
    if (want.id) {
      log("warn", "前回選択したオーディオデバイスが見つからないため、既定に戻しました。");
    }
  }
}

function selectedModel() {
  return state.models.find((m) => m.name === ui.model.value) ?? null;
}

function selectedAudioLabel() {
  const option = ui.audio.selectedOptions[0];
  if (!option) return "既定のデバイス";
  const group = option.parentElement?.label ?? "";
  return option.textContent === "既定のデバイスを使う"
    ? `既定の${group.startsWith("マイク") ? "マイク" : "スピーカー"}`
    : option.textContent;
}

/** Warn about model/device combinations known to fail on this hardware. */
function refreshModelNotice() {
  // A missing runtime outranks everything, but the setup banner already says so
  // and offers the fix; repeating it down here as an error is just noise.
  if (state.openvino && state.openvino.state !== "ready") {
    clearNotice();
    return;
  }
  const model = selectedModel();
  if (!model) {
    clearNotice();
    return;
  }
  if (!model.usable) {
    showNotice("error", model.problems.join(" "));
    return;
  }
  if (model.unsupportedDevices.includes(ui.device.value)) {
    showNotice("warn", model.warnings.join(" "));
    return;
  }
  clearNotice();
}

// --------------------------------------------------------- setup and updates

/** What is still missing, in the order the setup dialog asks for it. */
function missingPieces() {
  const missing = [];
  if (state.openvino?.state !== "ready") missing.push("OpenVINO ランタイム");
  if (!state.models.some((m) => m.usable)) missing.push("文字起こしモデル");
  return missing;
}

function renderBanners() {
  const setupBanner = el("setup-banner");
  const missing = missingPieces();
  setupBanner.classList.toggle("hidden", missing.length === 0);
  if (missing.length > 0) {
    el("setup-banner-text").textContent =
      `${missing.join("と")}がまだありません。セットアップからダウンロードしてください。`;
  }

  const updateBanner = el("update-banner");
  updateBanner.classList.toggle("hidden", !state.update);
  if (state.update) {
    el("update-banner-title").textContent = state.updating
      ? "更新中…"
      : `新しいバージョン ${state.update.version} があります`;
    el("update-banner-text").textContent = state.updating
      ? "ダウンロードが終わるとインストーラーが起動し、アプリが再起動します。"
      : (state.update.notes ?? `現在 ${state.update.currentVersion}`).split("\n")[0];
    el("update-now").disabled = state.updating;
    el("update-later").disabled = state.updating;
  }
}

function renderCatalog() {
  const host = el("catalog");
  host.replaceChildren();
  if (!state.setup) return;

  const busy = Boolean(state.setup.busy);
  for (const entry of state.setup.catalog) {
    const row = document.createElement("div");
    row.className = "catalog-row";

    const name = document.createElement("span");
    name.className = "name";
    name.textContent = entry.name;

    const size = document.createElement("span");
    size.className = "size";
    size.textContent = `${entry.sizeMb} MB`;

    const note = document.createElement("span");
    note.className = "note";
    note.textContent = entry.note;

    row.append(name, size, note);

    if (entry.recommended && !entry.installed) {
      const tag = document.createElement("span");
      tag.className = "tag";
      tag.textContent = "推奨";
      row.append(tag);
    }

    const action = document.createElement("button");
    action.className = entry.recommended ? "primary small" : "ghost small";
    if (entry.installed) {
      action.className = "ghost small";
      action.textContent = "取得済み";
      action.disabled = true;
    } else {
      action.textContent = "ダウンロード";
      action.disabled = busy;
      action.onclick = async () => {
        await call("download_model", { repo: entry.repo });
        await refreshSetup();
      };
    }
    row.append(action);
    host.append(row);
  }
}

function renderSetupDialog() {
  const setup = state.setup;
  if (!setup) return;

  el("setup-target-dir").textContent = setup.managedModelsDir;

  const ready = setup.openvinoReady;
  el("ov-state").textContent = ready
    ? `取得済み — ${setup.openvinoRoot}`
    : `未取得 — ${setup.openvinoSizeMb} MB (${setup.openvinoVersion}) をダウンロードします`;
  el("ov-download").textContent = ready ? "再取得" : "ダウンロード";
  el("ov-download").disabled = Boolean(setup.busy);
  el("step-openvino").classList.toggle("done", ready);

  const installed = setup.catalog.filter((entry) => entry.installed).length;
  el("model-state").textContent =
    installed > 0 ? `${installed} 件を取得済み` : "1 つ選んでダウンロードしてください";
  el("step-model").classList.toggle("done", installed > 0);

  el("setup-cancel").classList.toggle("hidden", !setup.busy);
  renderCatalog();
}

/** Move one of the two progress bars, or clear it. */
function renderSetupProgress(payload) {
  const prefix = payload.task === "openvino" ? "ov" : "model";
  const bar = el(`${prefix}-progress`);
  const fill = el(`${prefix}-progress-fill`);
  const text = el(`${prefix}-progress-text`);

  const running = payload.phase === "download" || payload.phase === "extract";
  bar.classList.toggle("hidden", !running);
  text.classList.toggle("error", payload.phase === "error");

  if (!running) {
    fill.style.width = "0%";
    text.textContent = payload.message ?? "";
    return;
  }

  const percent = payload.total > 0 ? (payload.received / payload.total) * 100 : 0;
  fill.style.width = `${Math.min(100, percent).toFixed(1)}%`;

  const bits = [payload.phase === "extract" ? "展開中" : "ダウンロード中"];
  if (payload.total > 0) {
    bits.push(`${mb(payload.received)} / ${mb(payload.total)} (${percent.toFixed(0)}%)`);
  } else {
    bits.push(mb(payload.received));
  }
  if (payload.filesTotal > 1) {
    bits.push(`${payload.filesDone} / ${payload.filesTotal} ファイル`);
  }
  if (payload.message) bits.push(payload.message);
  text.textContent = bits.join("  ·  ");
}

async function refreshSetup() {
  state.setup = await call("setup_status");
  renderSetupDialog();
  renderBanners();
}

/** Re-read the model list after a download, without disturbing the selection. */
async function refreshModels() {
  const listing = await call("list_models");
  state.modelsDir = listing.dir;
  state.models = listing.models;
  renderModels();
  refreshModelNotice();
  renderStatus();
}

async function onSetupProgress(payload) {
  renderSetupProgress(payload);
  if (payload.phase === "download" || payload.phase === "extract") return;

  // Terminal: the worker slot is free, so the buttons and the model list both
  // need to catch up with what is now on disk.
  if (payload.task === "model" && payload.phase === "done") await refreshModels();
  await refreshSetup();
}

async function checkForUpdate({ quiet } = {}) {
  const label = el("update-state");
  if (!quiet) label.textContent = "確認中…";
  try {
    const found = await invoke("check_update");
    state.update = found;
    if (!quiet) {
      label.textContent = found
        ? `バージョン ${found.version} が利用できます。`
        : `最新です (${state.version})。`;
    }
    renderBanners();
  } catch (error) {
    // Offline is the normal case for a laptop, not a red banner.
    log("warn", String(error));
    if (!quiet) label.textContent = String(error);
  }
}

async function installUpdate() {
  state.updating = true;
  renderBanners();
  try {
    // Does not return when it works: the installer takes over and relaunches.
    await invoke("install_update");
  } catch (error) {
    state.updating = false;
    renderBanners();
    showNotice("error", String(error));
  }
}

// ------------------------------------------------------------------- status

const PHASE_LABEL = {
  idle: "停止中",
  loadingModel: "モデル読み込み中",
  running: "文字起こし中",
  finalizing: "残りを処理中",
};

function renderStatus() {
  const status = state.status;
  const phase = status.phase ?? "idle";

  ui.phase.textContent = PHASE_LABEL[phase] ?? phase;
  ui.phase.className = `pill ${
    phase === "running" ? "running" : phase === "loadingModel" ? "loading" : phase === "finalizing" ? "finalizing" : "idle"
  }`;

  const bits = [];
  if (status.modelLoaded && status.model) {
    bits.push(
      `${status.model} を ${status.device} に常駐中` +
        (status.loadSeconds ? ` (読み込み ${status.loadSeconds.toFixed(0)} 秒)` : "")
    );
  } else if (phase === "loadingModel") {
    bits.push(`${status.model ?? "モデル"} をコンパイル中 — NPU では数分かかります`);
  }
  if (status.audioDevice) bits.push(status.audioDevice);
  if (status.queued > 0) bits.push(`未処理 ${status.queued} 発話`);
  if (status.dropped > 0) bits.push(`破棄 ${status.dropped} 発話`);
  ui.statusText.textContent = bits.join("  ·  ");

  const recording = state.recordingId !== null;
  const runtimeReady = !state.openvino || state.openvino.state === "ready";
  if (!runtimeReady) {
    ui.toggle.textContent = "● 開始";
    ui.toggle.disabled = true;
    ui.toggle.classList.remove("recording");
  } else if (phase === "finalizing") {
    ui.toggle.textContent = "処理中…";
    ui.toggle.disabled = true;
    ui.toggle.classList.remove("recording");
  } else if (recording) {
    ui.toggle.textContent = "■ 停止";
    ui.toggle.disabled = false;
    ui.toggle.classList.add("recording");
  } else {
    ui.toggle.textContent = "● 開始";
    ui.toggle.disabled = false;
    ui.toggle.classList.remove("recording");
  }

  const busy = recording || phase !== "idle";
  ui.preload.disabled = busy || !runtimeReady || !selectedModel()?.usable;
  ui.model.disabled = busy || state.models.length === 0;
  ui.device.disabled = busy;
  ui.audio.disabled = busy;

  if (status.error) showNotice("error", status.error);
}

function renderLogs() {
  ui.logPanel.replaceChildren();
  for (const entry of state.logs.slice(-200)) {
    const line = document.createElement("div");
    line.className = `log-line ${entry.level}`;
    const ts = document.createElement("span");
    ts.className = "ts";
    ts.textContent = `${pad(entry.at.getHours())}:${pad(entry.at.getMinutes())}:${pad(
      entry.at.getSeconds()
    )}`;
    line.append(ts, document.createTextNode(entry.message));
    ui.logPanel.append(line);
  }
  ui.logPanel.scrollTop = ui.logPanel.scrollHeight;
}

// ------------------------------------------------------------------ actions

async function startTranscription() {
  const model = selectedModel();
  if (!model) {
    showNotice("error", "モデルを選択してください。");
    return;
  }
  if (!model.usable) {
    showNotice("error", model.problems.join(" "));
    return;
  }

  const audio = parseDeviceValue(ui.audio.value);
  const settings = state.settings;

  const id = await call("start_transcription", {
    req: {
      modelDir: model.path,
      modelName: model.name,
      device: ui.device.value,
      language: settings.language,
      audio,
      audioDeviceName: selectedAudioLabel(),
      maxSegmentS: settings.maxSegmentS,
      vadThreshold: settings.vadThreshold,
      minSilenceMs: settings.minSilenceMs,
    },
  });

  state.recordingId = id;
  state.viewId = id;
  setHeader({
    id,
    title: RECORDING_TITLE,
    startedAt: new Date().toISOString(),
    model: model.name,
    device: ui.device.value,
    audioDevice: selectedAudioLabel(),
    segmentCount: 0,
    audioSeconds: 0,
    interrupted: false,
  });
  renderTranscript([]);
  await persistSelection();
  await refreshSessions();
  renderStatus();
}

async function stopTranscription() {
  await call("stop_transcription");
}

async function persistSelection() {
  const model = selectedModel();
  state.settings.model = model ? model.name : null;
  state.settings.device = ui.device.value;
  state.settings.audio = parseDeviceValue(ui.audio.value);
  await invoke("set_settings", { settings: state.settings }).catch((e) =>
    log("warn", `設定の保存に失敗しました: ${e}`)
  );
}

// -------------------------------------------------------------------- events

async function onStatus(status) {
  state.status = status;
  renderStatus();
}

async function onSegment(payload) {
  if (payload.sessionId !== state.viewId) {
    // Recording continues in the background while an old transcript is open.
    return;
  }
  appendSegment(payload.segment);
}

// Rebuilding the sidebar re-reads every session file, so coalesce: the list
// only needs to catch up with the running session's title and count, not track
// it utterance by utterance.
let sidebarRefresh = null;
function scheduleSidebarRefresh() {
  if (sidebarRefresh) return;
  sidebarRefresh = setTimeout(() => {
    sidebarRefresh = null;
    refreshSessions().catch(() => {});
  }, 4000);
}

// --------------------------------------------------------------------- setup

async function loadSettingsIntoDialog() {
  const s = state.settings;
  el("set-models-dir").value = state.modelsDir ?? "";
  el("set-openvino-dir").value = s.openvinoDir ?? "";
  const ov = state.openvino;
  el("set-openvino-state").textContent = !ov
    ? ""
    : ov.state === "ready"
      ? `— 検出済み: ${ov.root}`
      : "— 見つかりません";
  el("set-language").value = s.language;
  el("set-max-segment").value = s.maxSegmentS;
  el("set-threshold").value = s.vadThreshold;
  el("set-silence").value = s.minSilenceMs;
  el("set-autoscroll").checked = s.autoScroll;
  el("set-metrics").checked = s.showMetrics;
  el("set-preload-launch").checked = s.preloadOnLaunch;
  // The registry is the record, not settings.json: Task Manager can switch
  // the entry off behind the app's back.
  state.autostart = await invoke("autostart_enabled").catch(() => false);
  el("set-autostart").checked = state.autostart;
  el("set-version").textContent = state.version;
  el("update-state").textContent = state.update
    ? `バージョン ${state.update.version} が利用できます。`
    : "";
}

function wireEvents() {
  el("new-session").onclick = () => newSessionView();

  el("open-folder").onclick = async () => {
    const dir = await call("sessions_dir");
    await window.__TAURI__.opener.revealItemInDir(dir);
  };

  el("open-settings").onclick = async () => {
    await loadSettingsIntoDialog();
    el("settings-dialog").showModal();
  };

  const openSetup = async () => {
    await refreshSetup();
    el("setup-dialog").showModal();
  };
  el("open-setup").onclick = openSetup;
  el("open-setup-from-settings").onclick = async () => {
    el("settings-dialog").close();
    await openSetup();
  };
  el("setup-close").onclick = () => el("setup-dialog").close();

  el("ov-download").onclick = async () => {
    await call("download_openvino");
    await refreshSetup();
  };

  el("setup-cancel").onclick = async () => {
    await call("cancel_setup");
  };

  el("check-update").onclick = () => checkForUpdate({});
  el("update-now").onclick = () => installUpdate();
  el("update-later").onclick = () => {
    // Only for this run: the next launch checks again.
    state.update = null;
    renderBanners();
  };

  el("pick-models-dir").onclick = async () => {
    const picked = await window.__TAURI__.dialog.open({
      directory: true,
      title: "モデルフォルダを選択",
    });
    if (!picked) return;
    const listing = await call("set_models_dir", { dir: picked });
    state.modelsDir = listing.dir;
    state.models = listing.models;
    el("set-models-dir").value = listing.dir ?? "";
    renderModels();
    refreshModelNotice();
    renderStatus();
    // The catalog's "取得済み" marks are per-folder, so they move with it.
    await refreshSetup();
  };

  el("pick-openvino-dir").onclick = async () => {
    const picked = await window.__TAURI__.dialog.open({
      directory: true,
      title: "OpenVINO GenAI を展開したフォルダを選択",
    });
    if (!picked) return;
    // Rejects a folder that is not an install, so the mistake surfaces now
    // rather than as a silent failure on the next launch.
    await call("set_openvino_dir", { dir: picked });
    state.settings.openvinoDir = picked;
    el("set-openvino-dir").value = picked;
    showNotice(
      "warn",
      "OpenVINO の場所を保存しました。アプリを再起動すると反映されます。"
    );
  };

  el("settings-dialog").addEventListener("close", async (event) => {
    if (event.target.returnValue !== "save") return;
    state.settings.language = el("set-language").value.trim() || "ja";
    state.settings.maxSegmentS = Number(el("set-max-segment").value);
    state.settings.vadThreshold = Number(el("set-threshold").value);
    state.settings.minSilenceMs = Number(el("set-silence").value);
    state.settings.autoScroll = el("set-autoscroll").checked;
    state.settings.showMetrics = el("set-metrics").checked;
    state.settings.preloadOnLaunch = el("set-preload-launch").checked;
    await call("set_settings", { settings: state.settings });
    const autostart = el("set-autostart").checked;
    if (autostart !== state.autostart) {
      await call("set_autostart", { enabled: autostart });
      state.autostart = autostart;
      log(
        "info",
        autostart
          ? "サインイン時に起動するよう登録しました。"
          : "サインイン時の起動を解除しました。"
      );
    }
    if (state.viewId) await openSession(state.viewId);
  });

  ui.model.onchange = () => {
    refreshModelNotice();
    persistSelection();
    renderStatus();
  };
  ui.device.onchange = () => {
    refreshModelNotice();
    persistSelection();
  };
  ui.audio.onchange = () => persistSelection();

  el("refresh-audio").onclick = async () => {
    state.devices = await call("list_audio_devices");
    renderDevices();
    log("info", `オーディオデバイスを ${state.devices.length} 件検出しました。`);
  };

  ui.preload.onclick = async () => {
    const model = selectedModel();
    if (!model) return;
    await call("preload_model", {
      req: {
        modelDir: model.path,
        device: ui.device.value,
        language: state.settings.language,
      },
    });
  };

  ui.toggle.onclick = async () => {
    if (state.recordingId) await stopTranscription();
    else await startTranscription();
  };

  el("log-toggle").onclick = () => ui.logPanel.classList.toggle("hidden");

  el("rename").onclick = () => {
    el("rename-input").value = state.viewTitle ?? "";
    el("rename-dialog").showModal();
  };

  el("rename-dialog").addEventListener("close", async (event) => {
    if (event.target.returnValue !== "save") return;
    const title = el("rename-input").value.trim();
    if (!title || !state.viewId) return;
    await call("rename_session", { id: state.viewId, title });
    await refreshSessions();
    await openSession(state.viewId);
  });

  el("copy").onclick = async () => {
    if (!state.viewId) return;
    const markdown = await call("session_markdown", { id: state.viewId });
    await window.__TAURI__.clipboardManager.writeText(markdown);
    log("info", "文字起こしを Markdown としてコピーしました。");
  };

  el("export").onclick = async () => {
    if (!state.viewId) return;
    const path = await window.__TAURI__.dialog.save({
      title: "文字起こしを保存",
      defaultPath: `${(state.viewTitle ?? state.viewId).replace(/[\\/:*?"<>|]/g, "_")}.md`,
      filters: [{ name: "Markdown", extensions: ["md"] }],
    });
    if (!path) return;
    await call("save_session_as", { id: state.viewId, path });
    log("info", `${path} に書き出しました。`);
  };

  el("delete").onclick = async () => {
    if (!state.viewId) return;
    const ok = await window.__TAURI__.dialog.confirm?.(
      "この文字起こしを削除しますか？元に戻せません。"
    );
    // `confirm` is not part of the default dialog permission set; fall back to
    // the webview's own dialog if the plugin call is unavailable.
    if (ok === undefined ? !window.confirm("この文字起こしを削除しますか？") : !ok) {
      return;
    }
    await call("delete_session", { id: state.viewId });
    newSessionView();
    await refreshSessions();
  };
}

async function main() {
  wireEvents();

  state.version = await call("app_version");
  state.settings = await call("get_settings");
  state.openvino = await call("openvino_status");
  if (state.openvino.state !== "ready") {
    // Not an error the user has to act on any more: the setup panel can fix it,
    // and the banner says so. The searched paths still belong in the log.
    log("warn", state.openvino.message);
    for (const path of state.openvino.searched) log("info", `  探索: ${path}`);
  }
  const listing = await call("list_models");
  state.modelsDir = listing.dir;
  state.models = listing.models;

  try {
    state.devices = await invoke("list_audio_devices");
  } catch (error) {
    state.devices = [];
    log("error", `オーディオデバイスを列挙できません: ${error}`);
  }

  renderModels();
  renderDevices();
  refreshModelNotice();

  state.recordingId = await call("active_session");
  state.status = await call("get_status");
  await refreshSessions();

  if (state.recordingId) {
    await openSession(state.recordingId);
  } else {
    newSessionView();
  }
  renderStatus();

  await refreshSetup();
  // First run, or a fresh install: open the panel instead of making the user
  // find it. Anything already downloaded is reflected in the banner.
  if (missingPieces().length > 0) el("setup-dialog").showModal();

  await listen("mt://setup-progress", (event) => {
    onSetupProgress(event.payload).catch((error) => log("error", String(error)));
  });
  await listen("mt://openvino-status", async (event) => {
    state.openvino = event.payload;
    refreshModelNotice();
    renderStatus();
    await refreshSetup();
  });
  await listen("mt://update-available", (event) => {
    state.update = event.payload;
    renderBanners();
  });
  await listen("mt://update-progress", (event) => {
    const { phase, received, total, message } = event.payload;
    if (phase === "error") {
      state.updating = false;
      renderBanners();
      showNotice("error", message ?? "更新に失敗しました。");
      return;
    }
    const percent = total > 0 ? ` (${((received / total) * 100).toFixed(0)}%)` : "";
    el("update-banner-text").textContent =
      phase === "install"
        ? (message ?? "インストーラーを起動します…")
        : `ダウンロード中 ${mb(received)}${total > 0 ? ` / ${mb(total)}` : ""}${percent}`;
  });

  await listen("mt://status", (event) => onStatus(event.payload));
  await listen("mt://segment", (event) => {
    onSegment(event.payload);
    scheduleSidebarRefresh();
  });
  await listen("mt://log", (event) => log(event.payload.level, event.payload.message));
  await listen("mt://level", (event) => {
    const { rms, speech } = event.payload;
    // dBFS, floored at -60: linear RMS is unreadable as a bar.
    const db = 20 * Math.log10(Math.max(rms, 1e-6));
    const pct = Math.max(0, Math.min(100, ((db + 60) / 60) * 100));
    ui.meterFill.style.width = `${pct}%`;
    ui.meterFill.classList.toggle("speech", speech);
  });
  await listen("mt://session-finished", async (event) => {
    const { id, deleted } = event.payload;
    state.recordingId = null;
    if (deleted) {
      log("warn", "発話がなかったため、この記録は保存されませんでした。");
      if (state.viewId === id) newSessionView();
    } else if (state.viewId === id) {
      await openSession(id);
    }
    await refreshSessions();
    renderStatus();
  });
}

main().catch((error) => {
  document.body.textContent = `起動に失敗しました: ${error}`;
});
