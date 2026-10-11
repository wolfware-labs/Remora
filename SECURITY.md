# Security

## Reporting a vulnerability

Please report vulnerabilities privately, through
[GitHub's private vulnerability reporting](https://github.com/wolfware-labs/remora/security/advisories/new)
for this repository. Do not open a public issue.

Include what you found, how to reproduce it, and the Remora version (`remora --version`) and
operating system. You will get an answer within a week. Fixes are released as soon as they are
ready, and credited to you unless you prefer otherwise.

Only the latest release receives security fixes.

## What Remora protects

- **Your recorded history.** The journal is hash-chained and every entry is signed with a key
  that stays on your machine; `remora verify` reports the first entry that was changed,
  removed or reordered.
- **Your files at rest.** The data folder, the stored files (including a `.remora-store` folder
  on another volume) and the daemon's socket or named pipe are accessible to your user only.
  Remora refuses a store folder that other users could change.
- **Your privacy.** Remora makes no network calls and sends nothing anywhere.

## What it does not protect against

- Someone who can run code as your user, or as an administrator, can read your recorded files
  and the journal's signing key, and so can rewrite history and sign it again. The journal shows
  tampering by anything that does not have that key.
- Remora records the files an agent changes; it does not stop an agent from doing anything.
