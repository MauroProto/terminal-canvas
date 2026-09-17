# Security

TerminalCanvas executes real local terminals. Remote terminal control is an
explicit grant of the host OS account's authority, not a sandbox.

Keep invitation codes private, use a separate session passphrase, grant control
only to trusted participants, and close sharing when it is no longer needed.
Browser captures are untrusted data and require review and explicit copying;
they are never automatically submitted to a terminal.

Configuration and backups are created with private Unix permissions. On Windows,
store them in the standard per-user profile with a private inherited ACL.
Do not post config files, invitation codes, logs or diagnostic archives publicly
without reviewing their contents.

## Reporting

Use GitHub private vulnerability reporting when the repository exposes that
option. Otherwise contact the maintainer privately before publishing sensitive
reproduction details. This file does not assert that private reporting is enabled.

## Maintainer controls

Protect the default branch, require the CI checks, block force pushes and deletion,
and protect release tags. These are GitHub administration settings, not controls
that can be enabled by committing this file. Keep dependency advisory scans enabled.
