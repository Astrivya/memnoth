# memnoth-mcp-auth

Authentication verifier for the `memnoth-mcp` HTTP transport.

Three auth modes behind one `Authenticator` trait, all in-process, fail-closed:

| Mode | Header shape | Validates with | Audience | Issuer |
|---|---|---|---|---|
| `static_bearer` | `Authorization: Bearer <shared-secret>` | `constant_time_eq` vs `MEMNOTH_MCP_HTTP_TOKEN` | n/a | n/a |
| `hs256_user_jwt` | `Authorization: Bearer <jwt>` | HMAC-SHA256, `MEMNOTH_MCP_USER_TOKEN_SECRET` | `memnoth-mcp` | `astrivya-memnoth-install` |
| `rs256_zitadel_session` | `Authorization: Bearer <jwt>` | RS256, JWKS from `https://auth.astrivya.ai/oauth/v2/keys` (`MEMNOTH_MCP_ZITADEL_JWKS_URL`) | configurable | configurable |

Design doc: `D:\xclaw\memnoth-findings.md` §12.

## Hot-path targets

- Cached HS256 verify: **<10μs p99**
- Cached RS256 verify: **<50μs p99**
- JWKS cache miss (cold): one HTTP fetch, singleflight per `kid`

## Safety

`unsafe_code = "forbid"` at the crate level. Every code path is auditable Rust.
