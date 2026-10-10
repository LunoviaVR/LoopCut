#!/usr/bin/env node
// Verifies an updater signature the way the installed app will: the `.sig` next to an installer (base64 of a
// minisign signature, as `tauri build` and `tauri signer sign` write it) must be signed by the key whose public half is
// `plugins.updater.pubkey` in src-tauri/tauri.conf.json. Checks the key id, the file signature (Ed25519 over the
// BLAKE2b-512 hash for prehashed "ED" signatures, over the file itself for "Ed") and the trusted-comment signature.
//
// Usage: node scripts/verify-updater-signature.mjs <installer.exe> [installer.exe.sig]
// Exit code 0 when everything verifies, 1 otherwise.

import { createHash, createPublicKey, verify } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const [file, sigArg] = process.argv.slice(2);
if (!file) {
  console.error("usage: node scripts/verify-updater-signature.mjs <file> [file.sig]");
  process.exit(1);
}
const sigFile = sigArg ?? `${file}.sig`;

/** Lines of a base64-wrapped minisign text block. */
function minisignLines(base64Text) {
  return Buffer.from(base64Text.trim(), "base64").toString("utf8").trim().split(/\r?\n/);
}

const conf = JSON.parse(readFileSync(join(root, "src-tauri", "tauri.conf.json"), "utf8"));
// Public key: "Ed", 8-byte key id, 32-byte Ed25519 key.
const pub = Buffer.from(minisignLines(conf.plugins.updater.pubkey)[1], "base64");
// Signature: 2-byte algorithm, 8-byte key id, 64-byte signature; then the trusted comment and its signature.
const sigLines = minisignLines(readFileSync(sigFile, "utf8"));
const sig = Buffer.from(sigLines[1], "base64");
const trusted = sigLines[2].replace(/^trusted comment: /, "");
const trustedSig = Buffer.from(sigLines[3], "base64");

const key = createPublicKey({
  key: { kty: "OKP", crv: "Ed25519", x: pub.subarray(10, 42).toString("base64url") },
  format: "jwk",
});
const algorithm = sig.subarray(0, 2).toString("latin1");
const data = readFileSync(file);
const message = algorithm === "ED" ? createHash("blake2b512").update(data).digest() : data;

const keyIdMatches = pub.subarray(2, 10).equals(sig.subarray(2, 10));
const fileValid = verify(null, message, key, sig.subarray(10, 74));
const trustedValid = verify(
  null,
  Buffer.concat([sig.subarray(10, 74), Buffer.from(trusted, "utf8")]),
  key,
  trustedSig,
);

const keyId = Buffer.from(pub.subarray(2, 10)).reverse().toString("hex").toUpperCase();
console.log(`public key ${keyId} (tauri.conf.json) signed this file: ${keyIdMatches}`);
console.log(
  `file signature (${algorithm === "ED" ? "BLAKE2b-512 prehash" : "plain"}): ${fileValid}`,
);
console.log(`trusted comment "${trusted}": ${trustedValid}`);
process.exit(keyIdMatches && fileValid && trustedValid ? 0 : 1);
