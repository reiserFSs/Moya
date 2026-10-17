// Run with node ps-plugin/check.cjs; Photoshop itself exercises host operations.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");
const crypto = require("node:crypto");
const token = "0123456789abcdef0123456789abcdef";
const controls = new Map();
const availablePaths = new Set();
const requestedPaths = [];
let savedAppPath = null;
const context = vm.createContext({
  require: name => name === "photoshop" ? { app: {}, action: {}, core: {}, constants: {} }
    : name === "uxp" ? { storage: { localFileSystem: { async getEntryWithUrl(url) {
      requestedPaths.push(url);
      if (!availablePaths.has(url)) throw new Error("Not found");
      return { async read() { return JSON.stringify({ version: 2, port: 47811, token }); } };
    } } }, entrypoints: { setup() {} } }
    : { homedir: () => "/home/test" },
  document: { getElementById: id => { if (!controls.has(id)) controls.set(id, { addEventListener() {} }); return controls.get(id); } },
  localStorage: { getItem() { return savedAppPath; } }, console,
  crypto: crypto.webcrypto, setTimeout, clearTimeout,
});
vm.runInContext(fs.readFileSync(`${__dirname}/index.js`, "utf8"), context);
vm.runInContext(`
  const zip = new Uint8Array([80, 75, 3, 4, 0, 255, 128, 60, 38]);
  const project = encodeProject(zip.buffer);
  const packet = metadataPacket(project);
  const decoded = parseMetadata(packet);
  if (decoded.moya !== 2 || decoded.project !== project || String(new Uint8Array(decodeProject(decoded.project))) !== String(zip)) throw new Error("Project XMP roundtrip failed");
  const compact = '<rdf:Description xmlns:moya="' + namespace + '" moya:settings="' + xmlEscape(JSON.stringify(decoded)) + '"/>';
  if (JSON.stringify(parseMetadata(compact)) !== JSON.stringify(decoded)) throw new Error("Compact XMP roundtrip failed");
`, context);
assert.equal(vm.runInContext('parseMetadata("<rdf/>")', context), null);
assert.throws(() => vm.runInContext('parseMetadata(metadataPacket("bogus"))', context), /Invalid Moya project/);
assert.throws(() => vm.runInContext('parseMetadata(metadataPacket("AAAA"))', context), /Invalid Moya project/);
assert.throws(() => vm.runInContext('parseMetadata(metadataPacket(project).replace("&quot;moya&quot;:2", "&quot;moya&quot;:1"))', context), /Invalid Moya metadata/);
for (const length of [...Array.from({ length: 28 }, (_, i) => i + 4), 24577, 24578, 24579]) {
  const bytes = Buffer.concat([Buffer.from([80, 75, 3, 4]), crypto.randomBytes(length - 4)]);
  context.projectBytes = new Uint8Array(bytes);
  assert.equal(vm.runInContext("encodeProject(projectBytes.buffer)", context), bytes.toString("base64"));
  assert.deepEqual(Buffer.from(vm.runInContext("decodeProject(encodeProject(projectBytes.buffer))", context)), bytes);
}
vm.runInContext(`
  const current = { source: "/tmp/session.tif", returnMode: "layers", document: { depth: 16 } };
  const result = { depth: 16, returnMode: "layers", groupName: "Moya", projectPath: "/tmp/session.moya", composite: null, layers: [
    { path: "/tmp/session.moya-layer-1.tif", name: "Bottom", blend: "screen", opacity: 35, visible: false },
    { path: "/tmp/session.moya-layer-2.tif", name: "Top", blend: "lighten", opacity: 100, visible: true }
  ] };
  const planned = placementPlan(result, current);
  if (planned !== result.layers || planned[0].name !== "Bottom" || planned[1].name !== "Top") throw new Error("Bottom-to-top placement plan changed");
  if (planned[0].opacity !== 35 || planned[0].visible !== false || planned[1].blend !== "lighten") throw new Error("Layer properties lost");
  const compositeCurrent = { ...current, returnMode: "composite" };
  const compositeResult = { ...result, returnMode: "composite", layers: [], composite: "/tmp/session.moya.tif" };
  const compositePlan = placementPlan(compositeResult, compositeCurrent);
  if (compositePlan.length !== 1 || compositePlan[0].name !== "Moya" || compositePlan[0].blend !== "normal") throw new Error("Composite must be one Normal Moya layer");
`, context);
vm.runInContext(`
  for (const [oldKind, newKind, oldBlend, newBlend, expectedBlend] of [
    ["group", "pixel", "passThrough", "normal", "normal"],
    ["pixel", "group", "normal", "passThrough", "passThrough"],
    ["group", "group", "screen", "passThrough", "screen"],
  ]) {
    let blend = newBlend, assignments = 0;
    const old = { kind: oldKind, blendMode: oldBlend, opacity: 43, visible: false };
    const placed = { kind: newKind, opacity: 100, visible: true,
      get blendMode() { return blend; },
      set blendMode(value) {
        if (newKind === "pixel" && value === "passThrough") throw new Error("Pixel received group-only PassThrough");
        assignments++;
        blend = value;
      }
    };
    preserveAppearance(old, placed, false);
    if (placed.blendMode !== expectedBlend || placed.opacity !== 43 || placed.visible !== false) throw new Error("Replacement appearance was not preserved");
    if (assignments !== (oldKind === newKind ? 1 : 0)) throw new Error("Cross-kind replacement must retain native blend default");
  }
`, context);
for (const mutation of [
  'depth: 8', 'groupName: "Other"', 'projectPath: "/tmp/unrelated.moya"', 'composite: "/tmp/unrelated.tif"',
  'returnMode: "glow"', 'layers: [{ ...result.layers[0], path: "/tmp/unrelated.tif" }]',
  'layers: [{ ...result.layers[0], opacity: -1 }]', 'layers: [{ ...result.layers[0], opacity: 101 }]',
  'layers: [{ ...result.layers[0], visible: 1 }]', 'layers: [{ ...result.layers[0], blend: "multiply" }]',
]) assert.throws(() => vm.runInContext(`placementPlan({ ...result, ${mutation} }, current)`, context), /Invalid Moya|Invalid result/);
async function checkGroupTargets() {
  await vm.runInContext(`(async () => {
    const child = { id: 3 }, nested = { id: 2, layers: [child] }, group = { id: 1, layers: [nested] };
    const doc = { id: 10, layers: [group, { id: 4 }] };
    const requested = [];
    action.batchPlay = async commands => commands.map(command => {
      const id = command._target[1]._id;
      requested.push(id);
      return { XMPMetadataAsUTF8: id === 1 || id === 4 ? packet : "" };
    });
    const target = await reeditTarget(doc, child);
    if (target.layer !== group || requested.join(",") !== "3,2,1" || target.stored.project !== project) throw new Error("Child did not resolve to Moya group");
    requested.length = 0;
    if ((await reeditTarget(doc, group)).layer !== group || requested.join(",") !== "1") throw new Error("Selected group did not resolve");
    if ((await reeditTarget(doc, doc.layers[1])).layer.id !== 4) throw new Error("Composite metadata did not resolve");
    action.batchPlay = async () => [{ XMPMetadataAsUTF8: "" }];
    try { await reeditTarget(doc, child); throw new Error("Unmarked group accepted"); }
    catch (error) { if (!error.message.includes("Select a Moya group")) throw error; }
  })()`, context);
}
async function checkGroupPlacement() {
  await vm.runInContext(`(async () => {
    constants.ElementPlacement = { PLACEINSIDE: "placeInside", PLACEBEFORE: "placeBefore" };
    // Model both possible insertion positions; the final Photoshop stack must be top-first.
    for (const insertAtTop of [false, true]) {
      const group = { id: 100, layers: [] };
      for (const id of [1, 2, 3]) {
        const pixel = { id, async move(relative, placement) {
          const existing = group.layers.indexOf(this);
          if (existing !== -1) group.layers.splice(existing, 1);
          if (placement === "placeInside" && relative === group) {
            if (insertAtTop) group.layers.unshift(this);
            else group.layers.push(this);
          } else if (placement === "placeBefore" && group.layers.includes(relative)) {
            group.layers.splice(group.layers.indexOf(relative), 0, this);
          } else throw new Error("Unsupported native ElementPlacement: " + placement);
        } };
        await placePixelInGroup(pixel, group);
      }
      if (group.layers.map(layer => layer.id).join(",") !== "3,2,1") throw new Error("Bottom-to-top results did not become top-first group children");
    }
  })()`, context);
}
async function checkProjectCleanup() {
  await vm.runInContext(`(async () => {
    const original = fs.getEntryWithUrl, deleted = [];
    fs.getEntryWithUrl = async url => ({ async delete() { deleted.push(url); } });
    try {
      const current = { source: "/tmp/session.tif", projectPath: "/tmp/session.input.moya", document: { depth: 16 }, returnMode: "layers", resultPaths: ["/tmp/session.moya-layer-1.tif", "/tmp/session.moya-layer-2.tif"] };
      placementPlan(result, current);
      await cleanFiles(current);
      for (const suffix of [".tif", ".input.moya", ".moya", ".moya.tif", ".moya-layer-1.tif", ".moya-layer-2.tif"]) {
        if (deleted.filter(path => path === "file:/tmp/session" + suffix).length !== 1) throw new Error("Missing or repeated temporary cleanup: " + suffix);
      }
    } finally { fs.getEntryWithUrl = original; }
  })()`, context);
}
async function checkGroupDeletion() {
  await vm.runInContext(`(async () => {
    const original = action.batchPlay;
    let deleted = false;
    action.batchPlay = async commands => {
      if (commands.length !== 1 || commands[0]._obj !== "delete" || commands[0].deleteContained !== true) throw new Error("Replacement must delete group contents, not ungroup them");
      const target = commands[0]._target;
      if (target[0]._ref !== "layer" || target[0]._id !== 20 || target[1]._ref !== "document" || target[1]._id !== 10) throw new Error("Deletion targeted wrong layer or document");
      deleted = true;
      return [{}];
    };
    try {
      await deleteMoyaTarget(20, 10);
      if (!deleted) throw new Error("Old group was not deleted");
    } finally { action.batchPlay = original; }
  })()`, context);
}
for (const [text, digest] of [
  ["", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"],
  ["abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"],
  ["abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq", "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"],
  ["Moya 😃", crypto.createHash("sha256").update("Moya 😃").digest("hex")],
]) assert.equal(vm.runInContext(`sha256(${JSON.stringify(text)})`, context), digest);
const manifest = JSON.parse(fs.readFileSync(`${__dirname}/manifest.json`, "utf8"));
assert.equal(manifest.id, "local.moya.bridge");
assert.deepEqual(manifest.requiredPermissions.network.domains, ["ws://127.0.0.1:47811", "ws://localhost/"]);
assert.equal(manifest.entrypoints.filter(entry => entry.type === "command").length, 2);
async function checkAppPaths() {
  const systemApp = "/Applications/Moya.app";
  const userApp = "/home/test/Applications/Moya.app";
  const resolvePath = () => vm.runInContext("resolveAppPath()", context);
  availablePaths.add(`file:${systemApp}`);
  availablePaths.add(`file:${userApp}`);
  assert.equal(await resolvePath(), systemApp);
  assert.deepEqual(requestedPaths.splice(0), [`file:${systemApp}`]);
  availablePaths.delete(`file:${systemApp}`);
  assert.equal(await resolvePath(), userApp);
  assert.deepEqual(requestedPaths.splice(0), [`file:${systemApp}`, `file:${userApp}`]);
  availablePaths.clear();
  await assert.rejects(resolvePath(), /Install Moya/);
  assert.deepEqual(requestedPaths.splice(0), [`file:${systemApp}`, `file:${userApp}`]);
  availablePaths.add(`file:${systemApp}`);
  availablePaths.add(`file:${userApp}`);
  savedAppPath = "/custom/Moya.app";
  vm.runInContext('appPath.value = localStorage.getItem("moyaAppPath") || ""', context);
  assert.equal(await resolvePath(), savedAppPath);
  assert.deepEqual(requestedPaths.splice(0), []);
  controls.get("appPath").value = "~/Custom/Moya.app";
  assert.equal(await resolvePath(), "/home/test/Custom/Moya.app");
  assert.deepEqual(requestedPaths.splice(0), []);
}
async function checkServerProof() {
  availablePaths.add("file:/home/test/Library/Application Support/Moya/bridge.json");
  let responseProof, lastSocket;
  const nonces = new Set();
  context.WebSocket = class {
    constructor() {
      this.readyState = 1;
      this.sent = [];
      lastSocket = this;
      queueMicrotask(() => this.onopen());
    }
    send(data) {
      const message = JSON.parse(data);
      this.sent.push(message);
      if (message.type !== "hello") return;
      assert.match(message.nonce, /^[0-9a-f]{64}$/);
      assert.equal(nonces.has(message.nonce), false, "Handshake nonce must be fresh");
      nonces.add(message.nonce);
      assert.equal(message.token, token);
      const proof = crypto.createHash("sha256").update(token + message.nonce).digest("hex");
      assert.equal(message.version, 2);
      this.onmessage({ data: JSON.stringify({ type: "hello", version: 2, app: "moya", proof: responseProof === "valid" ? proof : responseProof }) });
      // Already queued frames must stay ignored even after a rejected handshake.
      if (responseProof !== "valid") {
        this.onmessage({ data: JSON.stringify({ type: "hello", version: 2, app: "moya", proof }) });
        this.onmessage({ data: JSON.stringify({ type: "apply", id: "untrusted" }) });
      }
    }
    close() { this.readyState = 3; }
  };
  vm.runInContext("let receivedMessages = 0; receive = async () => { receivedMessages++; };", context);
  for (responseProof of [undefined, "0".repeat(64)]) {
    await assert.rejects(vm.runInContext('connectOnce().then(() => send({ type: "edit" }))', context), /Invalid Moya handshake/);
    assert.equal(lastSocket.readyState, 3);
    assert.deepEqual(lastSocket.sent.map(message => message.type), ["hello"]);
    assert.equal(vm.runInContext("socket", context), undefined);
    assert.equal(vm.runInContext("receivedMessages", context), 0);
  }
  responseProof = "valid";
  for (let i = 0; i < 2; i++) {
    await vm.runInContext('connectOnce().then(() => send({ type: "edit" }))', context);
    assert.deepEqual(lastSocket.sent.map(message => message.type), ["hello", "edit"]);
    lastSocket.close();
  }
}
checkGroupTargets().then(checkGroupPlacement).then(checkGroupDeletion).then(checkProjectCleanup).then(checkAppPaths).then(checkServerProof)
  .then(() => console.log("Moya plugin v2 project XMP, placement planning, group targeting, manifest, app path, SHA-256 and server proof checks passed"))
  .catch(error => { console.error(error); process.exitCode = 1; });
