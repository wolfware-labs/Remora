# Security policy

## Supported versions

Remora has no release yet. Security fixes land on `main`, and in the latest published version once releases start.

## Reporting a vulnerability

Report vulnerabilities privately through GitHub: open the repository's **Security** tab and choose **Report a vulnerability**. Do not open a public issue for a security problem.

A useful report includes:

- the affected version or commit,
- the platform and filesystem, if the problem depends on them,
- steps that reproduce the problem,
- the impact you expect, for example another user reading recorded files, or a rewind writing outside the project.

You can expect a first reply within 7 days. Once a fix is ready, the advisory is published together with the release that contains it, and reporters are credited unless they ask not to be.

## Scope

The threat model is in [section 15 of the specification](docs/design/remora.md#15-security). In short:

- **In scope:** another local user reading recorded files, the journal or the keys; connecting to or impersonating the daemon; planting files that Remora later trusts. A process of the same user that causes damage by accident, for example a rewind that writes through a symlink or into an excluded folder.
- **Best effort:** a malicious process running as the same user. It can already read and change everything the user can.
- **Out of scope:** an administrator or root. Remora has no network surface: it opens no listening socket and makes no outgoing connection.

Bugs in third-party dependencies should be reported upstream, but let us know if Remora is affected.
