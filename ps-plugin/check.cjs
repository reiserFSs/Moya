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
      return { async read() { return JSON.stringify({ version: 1, port: 47811, token }); } };
    } } }, entrypoints: { setup() {} } }
    : { homedir: () => "/home/test" },
  document: { getElementById: id => { if (!controls.has(id)) controls.set(id, { addEventListener() {} }); return controls.get(id); } },
  localStorage: { getItem() { return savedAppPath; } }, console,
  crypto: crypto.webcrypto, setTimeout, clearTimeout,
});
vm.runInContext(fs.readFileSync(`${__dirname}/index.js`, "utf8"), context);
vm.runInContext(`
  const settings = { text: '<tag a="x"> & apostrophe' + String.fromCharCode(39) + ' / 😃', threshold: 0.5 };
  const packet = metadataPacket(settings, "glow");
  const decoded = parseMetadata(packet);
  if (JSON.stringify(decoded.params) !== JSON.stringify(settings) || decoded.returnMode !== "glow") throw new Error("Metadata roundtrip failed");
  const compact = '<rdf:Description xmlns:moya="' + namespace + '" moya:settings="' + xmlEscape(JSON.stringify(decoded)) + '"/>';
  if (JSON.stringify(parseMetadata(compact)) !== JSON.stringify(decoded)) throw new Error("Compact XMP roundtrip failed");
`, context);
assert.throws(() => vm.runInContext('parseMetadata("<rdf/>")', context), /Select a Moya layer/);
assert.throws(() => vm.runInContext('parseMetadata(metadataPacket({}, "bogus"))', context), /Invalid Moya/);
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
      this.onmessage({ data: JSON.stringify({ type: "hello", version: 1, app: "moya", proof: responseProof === "valid" ? proof : responseProof }) });
      // Already queued frames must stay ignored even after a rejected handshake.
      if (responseProof !== "valid") {
        this.onmessage({ data: JSON.stringify({ type: "hello", version: 1, app: "moya", proof }) });
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
checkAppPaths().then(checkServerProof)
  .then(() => console.log("Moya plugin metadata, manifest, app path, SHA-256 and server proof checks passed"))
  .catch(error => { console.error(error); process.exitCode = 1; });
