# Security policy

## Supported versions

Only the latest release gets fixes. Installed copies update themselves, so a fix reaches everyone through the normal update.

## Reporting a vulnerability

Please do not open a public issue. Report it privately through [GitHub's security advisories](https://github.com/klNuno/cortisoff/security/advisories/new), with the version, the system and the steps to reproduce.

You should get a first answer within a week. Once a fix is released, the advisory is published with credit to you unless you ask otherwise.

## What is in scope

- The daemon and its local pipe or socket: another user or a sandboxed process reaching it, or a request that makes it act outside what the protocol allows.
- The updater: anything that gets an unsigned or tampered build installed. Releases are signed with minisign and checked against the public key compiled into the app before anything is replaced.
- The plugin runtime: a plugin escaping the capabilities and API it is given.

Plugins run with your own rights and can start programs, type keys and change your screens. Installing one is trusting its author, the same as running any program; a plugin doing what its code says is not a vulnerability.
