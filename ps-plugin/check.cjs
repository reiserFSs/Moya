// Run with node ps-plugin/check.cjs; Photoshop itself exercises host operations.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");
const controls = new Map();
const availablePaths = new Set();
const requestedPaths = [];
let savedAppPath = null;
const context = vm.createContext({
  require: name => name === "photoshop" ? { app: {}, action: {}, core: {}, constants: {} }
    : name === "uxp" ? { storage: { localFileSystem: { async getEntryWithUrl(url) {
      requestedPaths.push(url);
      if (!availablePaths.has(url)) throw new Error("Not found");
      return {};
    } } }, entrypoints: { setup() {} } }
    : { homedir: () => "/home/test" },
  document: { getElementById: id => { if (!controls.has(id)) controls.set(id, { addEventListener() {} }); return controls.get(id); } },
  localStorage: { getItem() { return savedAppPath; } }, console,
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
checkAppPaths().then(() => console.log("Moya plugin metadata, manifest and app path checks passed"))
  .catch(error => { console.error(error); process.exitCode = 1; });
