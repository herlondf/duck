// Token OAuth2 de conta de servico do Google: JWT RS256 assinado com node:crypto.
import { createSign } from "node:crypto";

export type ServiceAccount = { client_email: string; private_key: string; token_uri?: string };

const b64url = (b: Buffer | string) => Buffer.from(b).toString("base64url");

export function signJwt(sa: ServiceAccount, scope: string, now = Math.floor(Date.now() / 1000)): string {
  const head = b64url(JSON.stringify({ alg: "RS256", typ: "JWT" }));
  const claims = b64url(JSON.stringify({
    iss: sa.client_email, scope, aud: sa.token_uri ?? "https://oauth2.googleapis.com/token", iat: now, exp: now + 3600,
  }));
  const sig = createSign("RSA-SHA256").update(`${head}.${claims}`).sign(sa.private_key);
  return `${head}.${claims}.${b64url(sig)}`;
}
