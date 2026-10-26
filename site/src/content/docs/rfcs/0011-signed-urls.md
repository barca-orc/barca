---
title: 'RFC-0011: Signed URLs'
description: 'barca.signed_url(uri) returns a time-limited HTTPS link to any object in S3, GCS or Azure using the credentials barca already uses, and barca get -o url returns one for a result.'
---

- **Status:** Implemented (0.26.0)
- **Date:** 2026-10-20 (accepted 2026-10-20)
- **Touches:** python/barca | barca-cli
- **Supersedes / Related:** Builds on the output modes of #404/#287 (`-o path`, `-o value`) and the store states of #390/#403 (`unsynced`, `local only`). [RFC-0009](/rfcs/0009-store-backup-and-restore/) for the store layout.

---

## 1. Summary

Two pieces:

1. **`barca.signed_url(uri, expires=3600)`** in Python returns a time-limited HTTPS URL that
   anyone can `GET` without credentials, for any object in S3 (and R2, MinIO), GCS or Azure,
   using the same credential chain barca already uses for its store. It is generic: it signs
   artifacts, sink outputs, backups or any object the caller can name.
2. **`barca get <target> -o url [--expires 1h]`** prints a signed URL for each result of the
   target instead of its path, so a result can be handed to a person or a service.

Read-only links only. No upload URLs, no listing.

## 2. Motivation

Results and sink outputs end up in buckets, and the people and services that need them
mostly have no cloud credentials: a stakeholder who wants the CSV, a BI tool that loads a
parquet file over HTTPS, a Slack message that should link to the report. Today that means
writing the signing code by hand in each project, once per cloud.

**fsspec's generic `sign()` is not enough.** All bucket access in barca goes through fsspec
(`python/barca/_storage.py`), which has a `sign()` method, but what it does depends on the
backend and on the credential:

| Store | fsspec `sign()` | With barca's default credentials |
|---|---|---|
| S3, R2, MinIO (s3fs) | presigns with the boto credential chain | works, including instance roles and SSO sessions |
| GCS (gcsfs) | `google-cloud-storage` `generate_signed_url` | works with a service-account key file; fails with workload identity, GCE metadata or user credentials, which cannot sign locally |
| Azure (adlfs) | builds a SAS from an account key or connection string only | fails: barca authenticates with `DefaultAzureCredential`, so there is no key |

The Azure case is the common one for deployments that follow the docs, and it fails. So the
capability belongs in barca's storage module, per backend, not as a call to `fs.sign`.

## 3. Guide-Level Explanation

### 3.1 Python

```python
import barca

url = barca.signed_url("s3://reports/weekly/2026-10-19.csv")            # 1 hour
url = barca.signed_url("abfss://out@acct.dfs.core.windows.net/x.parquet", expires=86400)
```

It works anywhere: a script, a notebook, or inside a `@task` that posts a link somewhere.
`expires` is seconds or a `datetime.timedelta`. It raises `barca.BarcaError` naming the URI
and the reason (no credentials that can sign, object does not exist, expiry too long). It
does not import or start the barca binary; it only needs the store extras.

### 3.2 CLI

```
$ barca get weekly_report pipeline.py -o url --expires 1d
https://reports.s3.amazonaws.com/barca/prod/artifacts/pipeline.py%3Aweekly_report/9f2c….csv?X-Amz-…
```

- One URL per line, one per target and per selected partition key, in the same order as
  `-o path`.
- With `--json`, each `_barca_artifact` pointer in `final_output` gains `url` and
  `url_expires_at` (RFC 3339).
- `--expires` takes `30m`, `12h`, `7d` or seconds; default `1h`; at most `7d`. It is a usage
  error without `-o url`.
- The target is brought up to date first, exactly as `barca get` does, so the link is to the
  current result.

### 3.3 HTTP API and UI

Not in this RFC. `GET /runs/{id}` and the run contract are being reworked by #393; a signed
URL in the HTTP surface and a download button in the UI belong with that work.

## 4. Reference-Level Explanation

### 4.1 Public API Surface

- **Python:** `barca.signed_url(uri: str, expires: int | timedelta = 3600) -> str`, exported
  from `barca`. Experimental.
- **CLI:** `url` as a value of `get -o`, and `--expires` on `get`. Both experimental in
  `contract.md`, in `EXPERIMENTAL` in `contract.rs`, with `--help` examples and snapshots.
- **JSON:** `final_output…_barca_artifact.url` and `.url_expires_at`, present only under
  `-o url`.
- **Exit codes:** a store that is not a cloud bucket (no store, a local directory, `file://`)
  is a usage error, exit 2, naming the store and why. A credential that cannot sign, or a
  result that is not in the store after the command's retry of pending uploads, is exit 3
  naming the node and the reason, with the `barca doctor` remediation line.
- **Never logged.** A signed URL is a bearer credential. It goes to stdout only. It is not
  written to stderr, the metadata database, telemetry, run history or `barca logs`.
  `_storage.safe_error` already strips signed query strings from diagnostics; tests assert it
  for all three formats.

### 4.2 Per-backend signing

All in `python/barca/_storage.py`, one function per scheme, chosen by the URI scheme:

- **S3 (`s3://`, `s3a://`, R2, MinIO):** `generate_presigned_url("get_object")` through the
  s3fs client, honouring `BARCA_STORAGE_OPTIONS` (endpoint URL for R2 and MinIO). SigV4 limits
  expiry to 7 days, and a URL signed with temporary credentials stops working when they expire;
  when the credential's own expiry is sooner than `expires`, the URL's real expiry is reported
  in `url_expires_at` and a warning says so.
- **GCS (`gs://`, `gcs://`):** V4 signing through `google-cloud-storage`. With a
  service-account key, signed locally. Otherwise the IAM `signBlob` path: pass the credential's
  service-account email and access token to `generate_signed_url`, which needs the
  `iam.serviceAccounts.signBlob` permission on that account (`roles/iam.serviceAccountTokenCreator`).
  User credentials from `gcloud auth application-default login` cannot sign either way; the error
  says to impersonate a service account.
- **Azure (`abfs://`, `abfss://`):** with an account key or connection string, a service SAS.
  Otherwise a **user delegation SAS**: `get_user_delegation_key` with the
  `DefaultAzureCredential` barca already uses, then `generate_blob_sas(user_delegation_key=…)`,
  read-only, for the blob. Needs `Microsoft.Storage/storageAccounts/blobServices/generateUserDelegationKey/action`
  (included in Storage Blob Data Contributor and Storage Blob Delegator). A delegation key lives
  at most 7 days. ADLS Gen2 paths sign as blobs; the URL uses the `blob.core.windows.net`
  endpoint.
- **Existence:** `signed_url` checks the object exists (one `HEAD`) before signing, so a typo
  does not become a link that 404s later.

### 4.3 Implementation Details

- `barca get -o url` resolves results exactly as `-o path` (#404), then signs each store
  object through the Python helper that `-o value` already starts (`python -m barca._value`
  from #447), extended with a `sign` request. One helper process per command, not per URL.
- A result whose store object is missing because its upload is pending (`unsynced`, `local
  only`, #390) is uploaded by the command's usual retry before signing. If it is still not in
  the store, exit 3 as above.
- Expiry parsing and the 7-day cap live in Rust for the CLI and in Python for the API, with a
  shared test table.

### 4.4 Rust ↔ Python Boundary

The `_value` helper protocol gains one request, `{"op": "sign", "uri", "expires"}`, answered
with `{"url", "expires_at"}` or an error with `kind`. Versioned under the worker protocol
negotiation (`specs/interfaces/worker-protocol.md`). No change to workers or to static analysis.

### 4.5 Edge Cases

- **A partitioned target** gets one URL per selected key. **A sink** is signed at the URI it
  wrote, when that URI is in a bucket.
- **The artifact changes after the link is made.** Artifact paths are `{node}/{run_hash}{ext}`,
  so a newer result is a new object and the old link keeps serving the old bytes until it
  expires. That is the right behaviour for a shared link, and the manual says so.
- **Clock skew** on the signing machine shortens or lengthens the real validity; not handled.
- **Revocation:** a signed URL cannot be revoked before it expires, except by rotating the key
  (S3, account-key SAS) or revoking delegation keys (Azure). The manual says so, and that is
  why the default is one hour.

## 5. Determinism, Caching & Testing

No effect on hashing or caching.

- MinIO in the `backends` CI job: a presigned URL fetched with plain `urllib` returns the
  object's bytes; after expiry it returns 403.
- Azurite: account-key SAS and user delegation SAS, where Azurite supports it; the delegation
  path is otherwise covered by a unit test of the request it builds.
- fake-gcs-server does not verify signatures: the GCS test checks the URL's shape, the
  `signBlob` call made for non-key credentials, and the error for user credentials.
- CLI: `-o url` on a cached asset, a partitioned target with `--partition`, a `local only`
  result that is uploaded then signed, a local store (exit 2), `--expires 8d` (exit 2),
  `--expires` without `-o url` (exit 2).
- The signed URL appears on stdout and in nothing else: stderr, `barca logs`, telemetry spans
  and the metadata database are searched for it.

## 6. Performance

Not on the hot path. One helper start and one `HEAD` plus one signing call per URL, only
under `-o url`. Azure fetches one delegation key per command.

## 7. Drawbacks

- Three code paths that cloud SDKs change under us; the emulators cover only part of them.
- A long-lived link is a leak waiting to happen; the 7-day cap and one-hour default limit it.
- GCS users on user credentials get an error until they set up impersonation.

## 8. Rationale & Alternatives

- **Call `fs.sign()` and stop.** Fails on Azure with the default credential and on GCS without
  a key file, which are the deployments most likely to want this.
- **Public-read objects or a public bucket.** No expiry, no revocation, and a store-wide
  setting for a per-object need.
- **Serve downloads through `barca serve`.** The server has no authentication by design (it
  sits behind a proxy), and streaming large artifacts through it is the opposite of what a
  bucket is for. A signed URL keeps the bytes between the client and the cloud.
- **A separate `barca url <target>` command.** `get -o url` reuses target resolution,
  `--partition` and freshness; a new command would duplicate them.

## 9. Prior Art

`aws s3 presign`, `gcloud storage sign-url`, `az storage blob generate-sas
--as-user --auth-mode login`. Dagster and Prefect leave this to user code.

## 10. Unresolved Questions

Decided 2026-10-20 (maintainer, taking each proposal): expiry defaults to 1 hour with a 7-day maximum; links set `Content-Disposition: attachment; filename=<node>.<ext>`; only `barca.signed_url` is added, `barca.get` keeps returning values; it ships in 0.26.0. It shipped in 0.26.0 as decided.

The options as they were put to the maintainer, kept for the record:

1. **Default and maximum expiry:** 1 hour and 7 days (proposed).
2. **Content-Disposition:** set `attachment; filename=<node>.<ext>` so a browser downloads with
   a readable name (proposed), or leave the object's own headers.
3. **`barca.get(..., output="url")`** in the Python API as well as `barca.signed_url`, or only
   the generic function (proposed: only `signed_url`; `barca.get` returns values).
4. **Release:** 0.26.0 with the store work, since it reads `unsynced` and pending uploads
   (#390) (proposed), or 0.25.x right after the output change (#447).

## 11. Future Possibilities

- A `url` field and a download link in the HTTP API and the UI, with #393.
- A signed link in Slack notifications (RFC-0010) for a task that publishes a report.
- Upload URLs for an external system to write into the store, if a need appears.
