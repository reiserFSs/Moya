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
// UXP exposes secure random values, but not SubtleCrypto.digest.
function sha256(text) {
  const bytes = Array.from(unescape(encodeURIComponent(text)), char => char.charCodeAt(0));
  const bitLength = bytes.length * 8;
  bytes.push(0x80);
  while (bytes.length % 64 !== 56) bytes.push(0);
  for (let i = 7; i >= 0; i--) bytes.push(Math.floor(bitLength / 2 ** (i * 8)) & 255);
  const hash = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
  const k = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
  ];
  const rotate = (n, bits) => (n >>> bits) | (n << (32 - bits));
  const words = new Int32Array(64);
  for (let offset = 0; offset < bytes.length; offset += 64) {
    for (let i = 0; i < 16; i++) {
      const j = offset + i * 4;
      words[i] = (bytes[j] << 24) | (bytes[j + 1] << 16) | (bytes[j + 2] << 8) | bytes[j + 3];
    }
    for (let i = 16; i < 64; i++) {
      const x = words[i - 15], y = words[i - 2];
      words[i] = words[i - 16] + (rotate(x, 7) ^ rotate(x, 18) ^ (x >>> 3)) + words[i - 7] + (rotate(y, 17) ^ rotate(y, 19) ^ (y >>> 10));
    }
    let [a, b, c, d, e, f, g, h] = hash;
    for (let i = 0; i < 64; i++) {
      const t1 = (h + (rotate(e, 6) ^ rotate(e, 11) ^ rotate(e, 25)) + ((e & f) ^ (~e & g)) + k[i] + words[i]) | 0;
      const t2 = ((rotate(a, 2) ^ rotate(a, 13) ^ rotate(a, 22)) + ((a & b) ^ (a & c) ^ (b & c))) | 0;
      h = g; g = f; f = e; e = (d + t1) | 0; d = c; c = b; b = a; a = (t1 + t2) | 0;
    }
    const state = [a, b, c, d, e, f, g, h];
    for (let i = 0; i < 8; i++) hash[i] = (hash[i] + state[i]) | 0;
  }
  return hash.map(word => (word >>> 0).toString(16).padStart(8, "0")).join("");
}
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
function encodeProject(buffer) {
  const bytes = new Uint8Array(buffer), alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const chunks = [];
  let chunk = "";
  for (let i = 0; i < bytes.length; i += 3) {
    const n = (bytes[i] << 16) | ((bytes[i + 1] || 0) << 8) | (bytes[i + 2] || 0);
    chunk += alphabet[n >>> 18] + alphabet[(n >>> 12) & 63] + (i + 1 < bytes.length ? alphabet[(n >>> 6) & 63] : "=") + (i + 2 < bytes.length ? alphabet[n & 63] : "=");
    if (chunk.length >= 32768) { chunks.push(chunk); chunk = ""; }
  }
  chunks.push(chunk);
  return chunks.join("");
}
function decodeProject(project) {
  if (typeof project !== "string" || !project.length || project.length % 4 || !/^[A-Za-z0-9+/]*={0,2}$/.test(project)) throw new Error("Invalid Moya project metadata.");
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const bytes = new Uint8Array(project.length / 4 * 3 - (project.endsWith("==") ? 2 : project.endsWith("=") ? 1 : 0));
  let offset = 0;
  for (let i = 0; i < project.length; i += 4) {
    const n = (alphabet.indexOf(project[i]) << 18) | (alphabet.indexOf(project[i + 1]) << 12) | (Math.max(0, alphabet.indexOf(project[i + 2])) << 6) | Math.max(0, alphabet.indexOf(project[i + 3]));
    for (const shift of [16, 8, 0]) if (offset < bytes.length) bytes[offset++] = (n >>> shift) & 255;
  }
  if (bytes[0] !== 80 || bytes[1] !== 75 || bytes[2] !== 3 || bytes[3] !== 4) throw new Error("Invalid Moya project archive.");
  return bytes.buffer;
}
function metadataPacket(project) {
  const json = xmlEscape(JSON.stringify({ moya: 2, project }));
  return `<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:moya="${namespace}"><moya:settings>${json}</moya:settings></rdf:Description></rdf:RDF></x:xmpmeta>`;
}
function parseMetadata(packet) {
  const match = packet && (packet.match(/<moya:settings(?:\s[^>]*)?>([\s\S]*?)<\/moya:settings>/) || packet.match(/\bmoya:settings="([^"]*)"/) || packet.match(/\bmoya:settings='([^']*)'/));
  if (!match || !packet.includes(namespace)) return null;
  const json = match[1].replace(/&quot;/g, '"').replace(/&apos;/g, "'").replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&amp;/g, "&");
  const value = JSON.parse(json);
  if (value.moya !== 2) throw new Error("Invalid Moya metadata.");
  decodeProject(value.project);
  return value;
}
function layerAncestors(layers, id, ancestors = []) {
  for (const layer of layers) {
    const path = [layer, ...ancestors];
    if (layer.id === id) return path;
    if (layer.layers) { const found = layerAncestors(layer.layers, id, path); if (found) return found; }
  }
}
async function reeditTarget(doc, layer) {
  for (const candidate of layerAncestors(doc.layers, layer.id) || []) {
    const stored = await layerMetadata(candidate.id, doc.id);
    if (stored) return { layer: candidate, stored };
  }
  throw new Error("Select a Moya group or one of its layers to re-edit.");
}
function placementPlan(message, current) {
  const stem = current.source.replace(/\.tif$/, "");
  if (message.depth !== current.document.depth || message.returnMode !== current.returnMode || message.groupName !== "Moya" || message.projectPath !== `${stem}.moya` || !Array.isArray(message.layers)) throw new Error("Invalid result from Moya.");
  if (message.returnMode === "composite") {
    if (message.composite !== `${stem}.moya.tif` || message.layers.length) throw new Error("Invalid Moya composite result.");
    return [{ path: message.composite, name: "Moya", blend: "normal", opacity: 100, visible: true }];
  }
  if (message.returnMode !== "layers" || message.composite !== null) throw new Error("Invalid Moya layers result.");
  const paths = new Set();
  for (let i = 0; i < message.layers.length; i++) {
    const layer = message.layers[i];
    if (layer.path !== `${stem}.moya-layer-${i + 1}.tif` || paths.has(layer.path) || typeof layer.name !== "string" || !["screen", "linearDodge", "lighten", "normal"].includes(layer.blend) || !Number.isFinite(layer.opacity) || layer.opacity < 0 || layer.opacity > 100 || typeof layer.visible !== "boolean") throw new Error("Invalid Moya layer result.");
    paths.add(layer.path);
  }
  return message.layers;
}
async function placePixelInGroup(pixel, group) {
  await pixel.move(group, constants.ElementPlacement.PLACEINSIDE);
  const top = group.layers[0];
  if (top.id !== pixel.id) await pixel.move(top, constants.ElementPlacement.PLACEBEFORE);
}
async function deleteMoyaTarget(layerId, documentId) {
  await batch([{ _obj: "delete", _target: [{ _ref: "layer", _id: layerId }, { _ref: "document", _id: documentId }], deleteContained: true }]);
}
function preserveAppearance(old, placed, wasVisible) {
  if (old.kind === placed.kind) placed.blendMode = old.blendMode;
  placed.opacity = old.opacity;
  placed.visible = wasVisible;
}
async function layerMetadata(layerId, documentId) {
  const [value] = await batch([{ _obj: "get", _target: [{ _property: "XMPMetadataAsUTF8" }, { _ref: "layer", _id: layerId }, { _ref: "document", _id: documentId }] }]);
  return parseMetadata(value.XMPMetadataAsUTF8);
}
async function cleanFiles(current) {
  for (const path of new Set([current.source, current.projectPath, current.source.replace(/\.tif$/, ".moya"), current.source.replace(/\.tif$/, ".moya.tif"), ...(current.resultPaths || [])].filter(Boolean))) {
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
  try { await restoreLayer(current); }
  finally { await cleanFiles(current); if (session === current) session = null; }
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
  status("Placing Moya…", "busy");
  try {
    const layers = placementPlan(message, current);
    current.resultPaths = layers.map(layer => layer.path);
    const project = encodeProject(await (await fileAt(message.projectPath)).read({ format: storage.formats.binary }));
    decodeProject(project);
    const target = app.documents.find(doc => doc.id === current.document.id);
    if (!target) throw new Error("The original Photoshop document was closed.");
    await core.executeAsModal(async context => {
      const history = await context.hostControl.suspendHistory({ documentID: target.id, name: "Moya" });
      let resultDoc;
      try {
        await selectDocument(target.id);
        const old = current.layerId ? findLayer(target.layers, current.layerId) : null;
        if (current.layerId && !old) throw new Error("The Moya group was deleted while editing.");
        let placed = message.returnMode === "layers" ? await target.createLayerGroup({ name: "Moya" }) : null;
        for (const spec of layers) {
          resultDoc = await app.open(await fileAt(spec.path));
          await context.hostControl.registerAutoCloseDocument(resultDoc.id);
          if (resultDoc.width !== target.width || resultDoc.height !== target.height || resultDoc.bitsPerChannel !== target.bitsPerChannel) throw new Error("Moya returned different image dimensions or depth.");
          const pixel = await resultDoc.layers[0].duplicate(target);
          await resultDoc.closeWithoutSaving();
          resultDoc = null;
          await selectDocument(target.id);
          pixel.name = spec.name;
          pixel.blendMode = { screen: constants.BlendMode.SCREEN, linearDodge: constants.BlendMode.LINEARDODGE, lighten: constants.BlendMode.LIGHTEN, normal: constants.BlendMode.NORMAL }[spec.blend];
          pixel.opacity = spec.opacity;
          pixel.visible = spec.visible;
          if (placed) await placePixelInGroup(pixel, placed);
          else placed = pixel;
        }
        if (old) {
          await placed.move(old, constants.ElementPlacement.PLACEBEFORE);
          preserveAppearance(old, placed, current.wasVisible);
        } else if (target.layers[0].id !== placed.id) {
          await placed.move(target.layers[0], constants.ElementPlacement.PLACEBEFORE);
        }
        placed.name = "Moya";
        if (message.returnMode === "composite") placed.blendMode = constants.BlendMode.NORMAL;
        await batch([{ _obj: "set", _target: [{ _ref: "property", _property: "XMPMetadataAsUTF8" }, { _ref: "layer", _id: placed.id }, { _ref: "document", _id: target.id }], to: { _obj: "layer", XMPMetadataAsUTF8: metadataPacket(project) } }]);
        if (old) await deleteMoyaTarget(old.id, target.id);
        await context.hostControl.resumeHistory(history);
      } catch (error) {
        try { if (resultDoc) await resultDoc.closeWithoutSaving(); }
        finally { await context.hostControl.resumeHistory(history, false); }
        throw error;
      }
    }, { commandName: "Moya" });
    await cleanFiles(current);
    session = null;
    send({ type: "placed", id: current.id });
    status("Placed in Photoshop", "ok");
  } catch (error) {
    try { await restoreLayer(current); }
    catch (restoreError) { console.log("Moya visibility restore:", errorText(restoreError)); }
    await cleanFiles(current);
    status(errorText(error), "error");
    send({ type: "error", id: current.id, message: errorText(error) });
  }
}
async function connectOnce() {
  const info = JSON.parse(await (await fileAt(`${home}/Library/Application Support/Moya/bridge.json`)).read());
  if (info.version !== 2 || info.port !== 47811 || !/^[a-fA-F0-9]{32}$/.test(info.token)) throw new Error("Invalid Moya bridge discovery file.");
  const nonce = Array.from(crypto.getRandomValues(new Uint8Array(32)), byte => byte.toString(16).padStart(2, "0")).join("");
  const proof = sha256(info.token + nonce);
  return new Promise((resolve, reject) => {
    // UXP v5 cannot match numeric-IP network permissions; localhost reaches the loopback-only server.
    const ws = new WebSocket("ws://localhost:47811");
    let ready = false, failed = false;
    const fail = error => { failed = true; clearTimeout(timeout); ws.close(); reject(error); };
    const timeout = setTimeout(() => fail(new Error("Moya handshake timed out.")), 1200);
    ws.onopen = () => { if (!failed) ws.send(JSON.stringify({ type: "hello", version: 2, token: info.token, nonce, client: "moya-bridge" })); };
    ws.onmessage = event => {
      if (failed) return;
      try {
        const message = JSON.parse(event.data);
        if (!ready) {
          if (message.type !== "hello" || message.version !== 2 || message.app !== "moya" || message.proof !== proof) throw new Error("Invalid Moya handshake.");
          ready = true; clearTimeout(timeout); socket = ws; resolve(ws);
        } else receive(message).catch(error => status(errorText(error), "error"));
      } catch (error) { fail(error); }
    };
    ws.onerror = () => fail(new Error("Cannot connect to Moya."));
    ws.onclose = () => {
      clearTimeout(timeout);
      failed = true;
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
    current = { id, source: source.nativePath, projectPath: null, document: { id: doc.id, name: doc.name, depth, width: doc.width, height: doc.height }, returnMode: document.getElementById("composite").checked ? "composite" : "layers" };
    status("Exporting Photoshop composite…", "busy");
    await core.executeAsModal(async context => {
      let duplicate;
      try {
        await selectDocument(doc.id);
        if (reedit) {
          const selected = doc.activeLayers[0];
          if (!selected || doc.activeLayers.length !== 1) throw new Error("Select one Moya group or one of its layers to re-edit.");
          const { layer, stored } = await reeditTarget(doc, selected);
          current.layerId = layer.id;
          current.wasVisible = layer.visible;
          const projectFile = await temp.createFile(`${id}.input.moya`, { overwrite: false });
          current.projectPath = projectFile.nativePath;
          await projectFile.write(decodeProject(stored.project), { format: storage.formats.binary });
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
    send({ type: "edit", id: current.id, source: current.source, projectPath: current.projectPath, document: current.document, returnMode: current.returnMode });
    status("Waiting for Moya…", "active");
  } catch (error) {
    if (current) {
      try { await restoreLayer(current); }
      catch (restoreError) { console.log("Moya visibility restore:", errorText(restoreError)); }
      await cleanFiles(current);
    }
    status(errorText(error), "error");
  } finally { busy = false; }
}
document.getElementById("edit").addEventListener("click", () => edit());
document.getElementById("reedit").addEventListener("click", () => edit(true));
entrypoints.setup({ commands: { edit: () => edit(), reedit: () => edit(true) }, panels: { bridge: { show() {} } } });
