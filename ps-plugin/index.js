const { app, action, core, constants } = require("photoshop");
const { storage, shell, entrypoints } = require("uxp");
const fs = storage.localFileSystem;
const home = require("os").homedir();
const namespace = "https://local.moya/bridge/1.0/";
let socket, connecting, session, busy = false;
const statusBox = document.getElementById("statusBox");
const actionButtons = [document.getElementById("edit"), document.getElementById("reedit")];
// kind: idle | busy (Photoshop work in progress; actions disabled) | active (Moya session open) | ok | error
const status = (text, kind = "idle") => {
  document.getElementById("status").textContent = text;
  statusBox.setAttribute("data-kind", kind);
  for (const button of actionButtons) button.disabled = kind === "busy";
};
const errorText = error => error && error.message ? error.message : String(error);
const appPath = document.getElementById("appPath");
appPath.value = localStorage.getItem("moyaAppPath") || "";
appPath.addEventListener("change", () => localStorage.setItem("moyaAppPath", appPath.value));
const settingsBody = document.getElementById("settingsBody");
document.getElementById("settingsToggle").addEventListener("click", () => {
  const open = settingsBody.classList.toggle("open");
  document.getElementById("chevron").textContent = open ? "▼" : "▶";
});
const fileAt = path => fs.getEntryWithUrl(`file:${path}`);
async function resolveAppPath() {
  if (appPath.value) return appPath.value.replace(/^~(?=\/)/, home);
  for (const path of ["/Applications/Moya.app", `${home}/Applications/Moya.app`]) {
    try { await fileAt(path); return path; } catch (_) { /* Try the next install location. */ }
  }
  throw new Error("Install Moya in /Applications or ~/Applications, or set its path in Settings.");
}
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const send = message => { if (socket && socket.readyState === 1) socket.send(JSON.stringify(message)); };
async function batch(commands) {
  const results = await action.batchPlay(commands, {});
  const error = results.find(result => result._obj === "error");
  if (error) throw new Error(error.message || "Photoshop command failed");
  return results;
}
async function selectDocument(id) {
  await batch([{ _obj: "select", _target: [{ _ref: "document", _id: id }] }]);
}
function findLayer(layers, id) {
  for (const layer of layers) {
    if (layer.id === id) return layer;
    if (layer.layers) { const found = findLayer(layer.layers, id); if (found) return found; }
  }
}
function xmlEscape(text) {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&apos;");
}
function metadataPacket(params, returnMode) {
  const json = xmlEscape(JSON.stringify({ moya: 1, params, returnMode }));
  return `<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:moya="${namespace}"><moya:settings>${json}</moya:settings></rdf:Description></rdf:RDF></x:xmpmeta>`;
}
function parseMetadata(packet) {
  const match = packet && (packet.match(/<moya:settings(?:\s[^>]*)?>([\s\S]*?)<\/moya:settings>/) || packet.match(/\bmoya:settings="([^"]*)"/) || packet.match(/\bmoya:settings='([^']*)'/));
  if (!match || !packet.includes(namespace)) throw new Error("Select a Moya layer to re-edit.");
  const json = match[1].replace(/&quot;/g, '"').replace(/&apos;/g, "'").replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&amp;/g, "&");
  const value = JSON.parse(json);
  if (value.moya !== 1 || !value.params || !["glow", "composite"].includes(value.returnMode)) throw new Error("Invalid Moya layer metadata.");
  return value;
}
async function layerMetadata(layerId, documentId) {
  const [value] = await batch([{ _obj: "get", _target: [{ _property: "XMPMetadataAsUTF8" }, { _ref: "layer", _id: layerId }, { _ref: "document", _id: documentId }] }]);
  return parseMetadata(value.XMPMetadataAsUTF8);
}
async function cleanFiles(current) {
  for (const path of [current.source, current.source.replace(/\.tif$/, ".moya.tif")]) {
    try { await (await fileAt(path)).delete(); } catch (error) { console.log("Moya temporary file cleanup:", String(error)); }
  }
}
async function restoreLayer(current) {
  if (!current || !current.layerId) return;
  const doc = app.documents.find(doc => doc.id === current.document.id);
  if (!doc) return;
  await core.executeAsModal(async () => {
    const layer = findLayer(doc.layers, current.layerId);
    if (layer) layer.visible = current.wasVisible;
  }, { commandName: "Cancel Moya" });
}
async function cancel(current) {
  await restoreLayer(current);
  await cleanFiles(current);
  if (session === current) session = null;
}
async function receive(message) {
  const current = session;
  if (!current || message.id !== current.id) return;
  if (message.type === "opened") { current.opened = true; status("Open in Moya — tweak the image, then Send to Photoshop.", "active"); return; }
  if (message.type === "cancelled") { await cancel(current); status("Moya session cancelled."); return; }
  if (message.type === "error") {
    if (!current.opened) await cancel(current);
    status(`${message.message}${current.opened ? " — retry from Moya." : ""}`, "error");
    return;
  }
  if (message.type !== "result") return;
  status("Placing layer…", "busy");
  try {
    const expected = current.source.replace(/\.tif$/, ".moya.tif");
    if (message.path !== expected || message.depth !== current.document.depth || !["screen", "linearDodge", "normal"].includes(message.blend) || !["glow", "composite"].includes(message.returnMode) || !message.params) throw new Error("Invalid result from Moya.");
    const target = app.documents.find(doc => doc.id === current.document.id);
    if (!target) throw new Error("The original Photoshop document was closed.");
    await core.executeAsModal(async context => {
      const history = await context.hostControl.suspendHistory({ documentID: target.id, name: "Moya" });
      let resultDoc, placed;
      try {
        await selectDocument(target.id);
        const old = current.layerId ? findLayer(target.layers, current.layerId) : null;
        if (current.layerId && !old) throw new Error("The Moya layer was deleted while editing.");
        resultDoc = await app.open(await fileAt(message.path));
        await context.hostControl.registerAutoCloseDocument(resultDoc.id);
        if (resultDoc.width !== target.width || resultDoc.height !== target.height) throw new Error("Moya returned different image dimensions.");
        placed = await resultDoc.layers[0].duplicate(target);
        await resultDoc.closeWithoutSaving();
        resultDoc = null;
        await selectDocument(target.id);
        if (old) {
          await placed.move(old, constants.ElementPlacement.PLACEBEFORE);
          placed.name = old.name;
          placed.blendMode = old.blendMode;
          placed.opacity = old.opacity;
          placed.visible = current.wasVisible;
        } else {
          if (target.layers[0].id !== placed.id) await placed.move(target.layers[0], constants.ElementPlacement.PLACEBEFORE);
          placed.name = message.layerName || "Moya Glow";
          placed.blendMode = { screen: constants.BlendMode.SCREEN, linearDodge: constants.BlendMode.LINEARDODGE, normal: constants.BlendMode.NORMAL }[message.blend];
        }
        await batch([{ _obj: "set", _target: [{ _ref: "property", _property: "XMPMetadataAsUTF8" }, { _ref: "layer", _id: placed.id }, { _ref: "document", _id: target.id }], to: { _obj: "layer", XMPMetadataAsUTF8: metadataPacket(message.params, message.returnMode) } }]);
        if (old) await old.delete();
        await context.hostControl.resumeHistory(history);
      } catch (error) {
        if (resultDoc) await resultDoc.closeWithoutSaving();
        await context.hostControl.resumeHistory(history, false);
        throw error;
      }
    }, { commandName: "Moya" });
    await cleanFiles(current);
    session = null;
    send({ type: "placed", id: current.id });
    status("Placed in Photoshop", "ok");
  } catch (error) {
    await restoreLayer(current);
    status(errorText(error), "error");
    send({ type: "error", id: current.id, message: errorText(error) });
  }
}
async function connectOnce() {
  const info = JSON.parse(await (await fileAt(`${home}/Library/Application Support/Moya/bridge.json`)).read());
  if (info.version !== 1 || info.port !== 47811 || !/^[a-fA-F0-9]{32}$/.test(info.token)) throw new Error("Invalid Moya bridge discovery file.");
  return new Promise((resolve, reject) => {
    // UXP v5 cannot match numeric-IP network permissions; localhost reaches the loopback-only server.
    const ws = new WebSocket("ws://localhost:47811");
    const timeout = setTimeout(() => { ws.close(); reject(new Error("Moya handshake timed out.")); }, 1200);
    let ready = false;
    ws.onopen = () => ws.send(JSON.stringify({ type: "hello", version: 1, token: info.token, client: "moya-bridge" }));
    ws.onmessage = event => {
      try {
        const message = JSON.parse(event.data);
        if (!ready) {
          if (message.type !== "hello" || message.version !== 1 || message.app !== "moya") throw new Error("Invalid Moya handshake.");
          ready = true; clearTimeout(timeout); socket = ws; resolve(ws);
        } else receive(message).catch(error => status(errorText(error), "error"));
      } catch (error) { clearTimeout(timeout); ws.close(); reject(error); }
    };
    ws.onerror = () => { clearTimeout(timeout); reject(new Error("Cannot connect to Moya.")); };
    ws.onclose = () => {
      clearTimeout(timeout);
      if (!ready) reject(new Error("Moya connection closed."));
      if (socket === ws) { socket = null; status("Moya disconnected."); if (session) cancel(session).catch(error => status(errorText(error), "error")); }
    };
  });
}
async function connect() {
  if (socket && socket.readyState === 1) return socket;
  if (connecting) return connecting;
  connecting = (async () => {
    try { return await connectOnce(); } catch (_) { /* Launch only when no server answers. */ }
    status("Launching Moya…", "busy");
    const launchError = await shell.openPath(await resolveAppPath(), "Launch Moya to edit this Photoshop document.");
    if (launchError) throw new Error(launchError);
    const deadline = Date.now() + 15000;
    let lastError;
    do {
      await delay(300);
      try { return await connectOnce(); } catch (error) { lastError = error; }
    } while (Date.now() < deadline);
    throw lastError;
  })();
  try { return await connecting; } finally { connecting = null; }
}
async function edit(reedit = false) {
  if (busy) return;
  busy = true;
  let current;
  try {
    if (!app.documents.length) throw new Error("Open a Photoshop document first.");
    const doc = app.activeDocument;
    const depth = { [constants.BitsPerChannelType.EIGHT]: 8, [constants.BitsPerChannelType.SIXTEEN]: 16, [constants.BitsPerChannelType.THIRTYTWO]: 32 }[doc.bitsPerChannel];
    if (!depth) throw new Error("Moya requires an 8-, 16-, or 32-bit document.");
    await connect();
    if (session) await cancel(session);
    const id = "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, c => { const n = Math.floor(Math.random() * 16); return (c === "x" ? n : (n & 3) | 8).toString(16); });
    const temp = await fs.getTemporaryFolder();
    const source = await temp.createFile(`${id}.tif`, { overwrite: false });
    current = { id, source: source.nativePath, document: { id: doc.id, name: doc.name, depth, width: doc.width, height: doc.height }, params: null, returnMode: document.getElementById("composite").checked ? "composite" : "glow" };
    status("Exporting Photoshop composite…", "busy");
    await core.executeAsModal(async context => {
      let duplicate;
      try {
        await selectDocument(doc.id);
        if (reedit) {
          const layer = doc.activeLayers[0];
          if (!layer || doc.activeLayers.length !== 1) throw new Error("Select one Moya layer to re-edit.");
          const stored = await layerMetadata(layer.id, doc.id);
          current.layerId = layer.id;
          current.wasVisible = layer.visible;
          current.params = stored.params;
          current.returnMode = stored.returnMode;
          layer.visible = false;
        }
        duplicate = await doc.duplicate(`Moya export ${id}`, true);
        await context.hostControl.registerAutoCloseDocument(duplicate.id);
        if (duplicate.mode !== constants.DocumentMode.RGB) await duplicate.changeMode(constants.ChangeMode.RGB);
        await batch([{ _obj: "save", as: { _obj: "TIFF", byteOrder: { _enum: "platform", _value: "IBMPC" }, saveTransparency: true }, in: { _path: fs.createSessionToken(source), _kind: "local" }, copy: true, layers: false, alphaChannels: false, spotColor: false, embedProfiles: true, lowerCase: true }]);
      } finally {
        if (duplicate) await duplicate.closeWithoutSaving();
        await selectDocument(doc.id);
      }
    }, { commandName: "Export to Moya" });
    if (!socket || socket.readyState !== 1) throw new Error("Moya disconnected during export.");
    session = current;
    send({ type: "edit", ...current, layerId: undefined, wasVisible: undefined });
    status("Waiting for Moya…", "active");
  } catch (error) {
    if (current) { await restoreLayer(current); await cleanFiles(current); }
    status(errorText(error), "error");
  } finally { busy = false; }
}
document.getElementById("edit").addEventListener("click", () => edit());
document.getElementById("reedit").addEventListener("click", () => edit(true));
entrypoints.setup({ commands: { edit: () => edit(), reedit: () => edit(true) }, panels: { bridge: { show() {} } } });
