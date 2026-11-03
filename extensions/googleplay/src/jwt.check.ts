// npm test: assina com uma chave RSA nova e confere com a publica.
import { generateKeyPairSync, createVerify } from "node:crypto";
import * as assert from "node:assert";
import { signJwt } from "./jwt";

const { privateKey, publicKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
const sa = { client_email: "x@y.iam.gserviceaccount.com", private_key: privateKey.export({ type: "pkcs8", format: "pem" }).toString() };
const jwt = signJwt(sa, "https://www.googleapis.com/auth/androidpublisher", 1000);
const [h, c, s] = jwt.split(".");
assert.deepStrictEqual(JSON.parse(Buffer.from(h, "base64url").toString()), { alg: "RS256", typ: "JWT" });
const claims = JSON.parse(Buffer.from(c, "base64url").toString());
assert.strictEqual(claims.iss, sa.client_email);
assert.strictEqual(claims.aud, "https://oauth2.googleapis.com/token");
assert.strictEqual(claims.exp - claims.iat, 3600);
assert.ok(createVerify("RSA-SHA256").update(`${h}.${c}`).verify(publicKey, Buffer.from(s, "base64url")), "assinatura invalida");
assert.ok(!createVerify("RSA-SHA256").update(`${h}.${c}x`).verify(publicKey, Buffer.from(s, "base64url")), "aceitou payload alterado");
console.log("JWT RS256 ok");
