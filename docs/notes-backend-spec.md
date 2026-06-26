# Notes Backend HTTP Contract

This document specifies the HTTP contract that an external "notes backend"
server must implement so that a client can store and retrieve **authorship
notes** keyed by commit SHA. The contract is intentionally small: it is a
commit-addressable key/value store with bulk write and bulk read.

A reader implementing a server from scratch needs only this document. No
prior knowledge of the client is required.

---

## 1. Conceptual model

The server stores opaque UTF-8 strings ("note content") keyed by a hex commit
SHA. There is exactly one logical store per deployment.

- **Keys** are hex commit SHAs (lowercase hex). Validation requirements are
  given in §4.1; clients reject non-hex keys before sending.
- **Values** are arbitrary UTF-8 strings. The server **must not** parse,
  rewrite, or re-encode the value. Treat it as an opaque blob.
- **Cardinality**: each key holds at most one value. Writing the same key
  again replaces the previous value.

The server is the source of truth for the (key → value) mapping. Clients
maintain their own caches but always defer to the server's last-written
value when they sync.

---

## 2. Transport

- **Protocol**: HTTP/1.1 over TCP. TLS is recommended for any
  non-loopback deployment but not required by this spec.
- **Encoding**: All request and response bodies are JSON
  (`Content-Type: application/json`). Bodies are UTF-8.
- **Methods**: Only `GET` and `POST` are used. The server should respond
  `405 Method Not Allowed` (or `404`) for any other method on a known path.
- **Connections**: The server may use any connection model
  (keep-alive, close-after-response, HTTP/2, etc.). The client makes no
  assumption beyond standard HTTP semantics.

A request that does not match any of the endpoints in §3 must return `404`.

---

## 3. Endpoints

There are exactly two endpoints. Both live under the prefix `/worker/notes`.
A trailing slash on the path is permitted on both endpoints; servers must
treat `/worker/notes` and `/worker/notes/` as equivalent.

The client allows a path prefix on the configured base URL, so a server
implementor may host the endpoints under any subpath that suits their
deployment. For example, with the client configured against
`https://app.example.com/api/gitai`, the requests issued are
`POST https://app.example.com/api/gitai/worker/notes/upload` and
`GET  https://app.example.com/api/gitai/worker/notes/?commits=...`.
The `/worker/notes` suffix is fixed; everything before it is the
deployment's choice.

### 3.1 `POST /worker/notes/upload` — bulk write

Stores or replaces a batch of notes.

**Request body** (JSON):

```json
{
  "entries": [
    { "commit_sha": "<hex>", "content": "<opaque utf-8>" },
    { "commit_sha": "<hex>", "content": "<opaque utf-8>" }
  ]
}
```

| Field             | Type            | Required | Description                                  |
|-------------------|-----------------|----------|----------------------------------------------|
| `entries`         | array of object | yes      | The notes to write. May be empty.            |
| `entries[].commit_sha` | string     | yes      | Hex commit SHA. See §4.1.                    |
| `entries[].content`    | string     | yes      | Opaque UTF-8 note content. Any length ≥ 0.   |

**Semantics**:

- For each entry, the server **upserts** the (commit_sha → content) pair.
  Any prior value for that key is replaced.
- The operation is **idempotent**: replaying the exact same request
  yields the same final state.
- Atomicity across the batch is **not** required. Partial success is
  allowed (see `failure_count` below).
- Order within `entries` is not significant; the server may apply writes
  in any order.

**Successful response** (`200 OK`):

```json
{
  "success_count": 2,
  "failure_count": 0
}
```

| Field           | Type    | Description                                           |
|-----------------|---------|-------------------------------------------------------|
| `success_count` | integer | Number of entries that were stored.                   |
| `failure_count` | integer | Number of entries that could not be stored.           |

`success_count + failure_count` should equal `entries.length`.

**Error responses**:

| Status | When                                                         | Body                                |
|--------|--------------------------------------------------------------|-------------------------------------|
| `400`  | Body is not valid JSON, or does not match the schema above   | `{ "error": "<message>" }`          |
| `401`  | Authentication required and missing/invalid (see §5)         | `{ "error": "<message>" }`          |
| `5xx`  | Internal failure                                             | `{ "error": "<message>" }`          |

The client treats any non-`200` response as a retriable failure (with
backoff) **except** `400`, which it treats as a permanent failure for
that batch.

### 3.2 `GET /worker/notes/?commits=<sha1>,<sha2>,...` — bulk read

Looks up notes for a list of commit SHAs.

**Query parameters**:

| Name      | Type   | Required | Description                                                |
|-----------|--------|----------|------------------------------------------------------------|
| `commits` | string | yes      | Comma-separated list of hex commit SHAs. No whitespace.    |

The server should accept up to **100 commit SHAs per request**. Behavior
on more than 100 is implementation-defined (truncation, `400`, or fully
honoring the request are all permitted), but a compliant server should
not crash.

**Successful response** (`200 OK`) — at least one requested SHA is known:

```json
{
  "notes": {
    "<sha>": "<content>",
    "<sha>": "<content>"
  }
}
```

| Field       | Type   | Description                                                 |
|-------------|--------|-------------------------------------------------------------|
| `notes`     | object | Map from commit SHA (string) to note content (string).      |

Only SHAs that exist in the store appear as keys. Unknown SHAs are
silently omitted from the map; the client treats their absence as
"no note for that commit".

**Empty result** (`404 Not Found`) — none of the requested SHAs are known:

```json
{ "notes": {} }
```

The client treats `404` as success-with-empty. Servers may alternatively
return `200` with an empty `notes` map; both are acceptable. (The
reference server returns `404` to exercise the cold-miss path; production
servers may pick whichever they prefer.)

**Error responses**:

| Status | When                                            | Body                                |
|--------|-------------------------------------------------|-------------------------------------|
| `400`  | `commits` parameter is missing or malformed     | `{ "error": "<message>" }`          |
| `401`  | Authentication required and missing/invalid     | `{ "error": "<message>" }`          |
| `5xx`  | Internal failure                                | `{ "error": "<message>" }`          |

---

## 4. Validation rules

### 4.1 Commit SHA format

A `commit_sha` is a string consisting of lowercase or uppercase
hexadecimal characters: `[0-9a-fA-F]`. Servers should accept any length
from 4 to 64 characters; in practice clients always send 40-character
SHA-1 hashes, but the spec does not constrain length so future hash
algorithms are accommodated.

A server **must not** treat two SHAs that differ only in case as the
same key. Clients always normalize to a single case before sending, so
servers can perform exact-string comparison.

A server **may** reject a request with `400` if any provided SHA
contains non-hex characters.

### 4.2 Content size

A single note is typically a few KB but may grow into the low MBs in
extreme cases. Servers should accept individual note content of at
least **8 MB**. Servers should accept upload batches with a body of at
least **64 MB**. Servers exceeding their limits should respond `413`
with an error body.

### 4.3 Empty inputs

- `entries: []` is a valid upload request. The response is
  `{ "success_count": 0, "failure_count": 0 }`.
- `commits=` (empty value) on read may return either `200` with
  `notes: {}` or `404` with `notes: {}`. Both are conforming.

---

## 5. Authentication

Authentication is **optional at the protocol level** but expected in
production. When required, servers authenticate the client via one of
two HTTP request headers (the client always sends one of these when
configured with credentials):

| Header          | Format                | Notes                              |
|-----------------|-----------------------|------------------------------------|
| `X-API-Key`     | `<api-key>`           | Long-lived, per-account API key.   |
| `Authorization` | `Bearer <token>`      | Short-lived access token.          |

Servers that require authentication must respond `401 Unauthorized` for
unauthenticated requests, with a JSON error body. Authorization (which
keys a given principal may write to) is out of scope for this document.

A server that does not require authentication ignores both headers.

---

## 6. Concurrency, ordering, and durability

- **Last-writer-wins**: if two upload requests arrive concurrently with
  conflicting values for the same key, either value may win. Clients do
  not depend on a specific resolution rule, but the server must not
  produce a value that was never sent (no merging, no truncation).
- **Read-after-write**: a successful response to `POST /worker/notes/upload`
  implies the written entries are visible to subsequent
  `GET /worker/notes/` requests on the same logical store.
- **Durability**: by the time `200` is returned, written entries should
  be durable across server restarts. Clients treat successful uploads as
  permanent; replaying them after a server restart is allowed but not
  required.
- **No deletes, no list, no enumeration**: this spec does not define a
  delete or list-all endpoint. Clients never need to enumerate all keys
  or remove a key.

---

## 7. Versioning and forward compatibility

This document defines version 1 of the contract. There is no version
header. Clients and servers should ignore unknown JSON fields they do
not recognize so that fields can be added later without a hard break.

Future versions will either:

- Add new optional fields that older servers ignore (no break).
- Add new endpoints under `/worker/notes/...` (older clients won't call
  them, no break).
- Or, if a breaking change is needed, ship under a new path prefix
  (e.g. `/worker/notes/v2/...`) so v1 clients continue to work.

---

## 8. Worked examples

### 8.1 Upload two notes

Request:

```http
POST /worker/notes/upload HTTP/1.1
Host: notes.example.com
Content-Type: application/json
X-API-Key: sk_live_abc123
Content-Length: 178

{
  "entries": [
    { "commit_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "content": "note-a" },
    { "commit_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "content": "note-b" }
  ]
}
```

Response:

```http
HTTP/1.1 200 OK
Content-Type: application/json
Content-Length: 41

{"success_count":2,"failure_count":0}
```

### 8.2 Read three notes (one missing)

Request:

```http
GET /worker/notes/?commits=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa,bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb,cccccccccccccccccccccccccccccccccccccccc HTTP/1.1
Host: notes.example.com
X-API-Key: sk_live_abc123
```

Response (the third SHA is unknown and is omitted):

```http
HTTP/1.1 200 OK
Content-Type: application/json

{
  "notes": {
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa": "note-a",
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb": "note-b"
  }
}
```

### 8.3 Read against an empty store

Request:

```http
GET /worker/notes/?commits=cccccccccccccccccccccccccccccccccccccccc HTTP/1.1
Host: notes.example.com
```

Response:

```http
HTTP/1.1 404 Not Found
Content-Type: application/json

{ "notes": {} }
```

(`200` with `{ "notes": {} }` is equally acceptable here.)

### 8.4 Malformed upload

Request:

```http
POST /worker/notes/upload HTTP/1.1
Host: notes.example.com
Content-Type: application/json
Content-Length: 8

not json
```

Response:

```http
HTTP/1.1 400 Bad Request
Content-Type: application/json

{ "error": "invalid request body: expected value at line 1 column 1" }
```

---

## 9. Conformance checklist

A server is considered compliant when all of the following hold:

1. `POST /worker/notes/upload` with a valid body returns `200` and
   `success_count == entries.length` (in the absence of partial
   failures).
2. After a successful upload, `GET /worker/notes/?commits=<sha>` returns
   `200` with `notes[sha]` equal to the uploaded content, byte-for-byte.
3. `GET /worker/notes/?commits=<unknown-sha>` returns either `404` with
   `{ "notes": {} }` or `200` with `{ "notes": {} }`.
4. A bulk `GET` containing a mix of known and unknown SHAs returns
   `200` and includes only the known SHAs in the `notes` map.
5. Re-uploading the same `commit_sha` with new content overwrites the
   prior value (last-writer-wins).
6. A request with malformed JSON or a missing required field returns
   `400` with a JSON error body.
7. `POST /worker/notes/upload` and `GET /worker/notes/` both accept the
   path with and without a trailing slash.
8. Unknown paths return `404`.

A reference, in-memory implementation that satisfies this checklist is
maintained alongside the client in this repository for use as a local
test target.
