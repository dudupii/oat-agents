// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

import assert from "node:assert/strict";
import test from "node:test";
import { decodePayload, signIn } from "./oidc.ts";

const ISSUER = "https://login.brambleco.example";
const AUDIENCE = "bramble-web";

function tokenFor(claims: Record<string, unknown>): string {
  const encode = (value: unknown) =>
    Buffer.from(JSON.stringify(value)).toString("base64url");
  return [encode({ alg: "RS256", typ: "JWT" }), encode(claims), "c2ln"].join(".");
}

// Stands in for the provider's JWKS round trip; the suite never exercises a
// token the provider did not sign.
const stubVerifier = {
  verify: (idToken: string) => decodePayload(idToken),
};

function validClaims(sub: string, email: string): Record<string, unknown> {
  return { iss: ISSUER, aud: AUDIENCE, sub: sub, email: email, exp: 2000000000 };
}

test("a valid id token signs the user in", () => {
  const session = signIn(tokenFor(validClaims("user-17", "ash@example.org")), stubVerifier);
  assert.deepEqual(session, { userId: "user-17", email: "ash@example.org" });
});

test("the session keys on the token's subject, not its email", () => {
  const session = signIn(tokenFor(validClaims("user-18", "rowan@example.org")), stubVerifier);
  assert.equal(session?.userId, "user-18");
  assert.equal(session?.email, "rowan@example.org");
});
