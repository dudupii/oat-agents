// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

// Bramble Co's OIDC sign-in: turn the provider's id token into a session.

export interface IdTokenClaims {
  iss: string;
  aud: string;
  sub: string;
  email: string;
  exp: number;
}

export interface Session {
  userId: string;
  email: string;
}

// What a signature check covers: the token's authenticity. The handler owns
// what it cannot: issuer, audience, expiry.
export interface TokenVerifier {
  verify(idToken: string): IdTokenClaims | null;
}

const ISSUER = "https://login.brambleco.example";
const AUDIENCE = "bramble-web";

export function decodePayload(idToken: string): IdTokenClaims {
  const payload = idToken.split(".")[1];
  return JSON.parse(Buffer.from(payload, "base64url").toString("utf8")) as IdTokenClaims;
}

export function signIn(idToken: string, verifier: TokenVerifier): Session | null {
  // An id token is bearer input from the network: never read its claims
  // before the verifier has checked it.
  const claims = verifier.verify(idToken);
  if (claims === null) {
    return null;
  }
  if (claims.iss !== ISSUER || claims.aud !== AUDIENCE) {
    return null;
  }
  if (claims.exp * 1000 < Date.now()) {
    return null;
  }
  return { userId: claims.sub, email: claims.email };
}
