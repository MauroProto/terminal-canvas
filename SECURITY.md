# Security and trust boundaries

TerminalCanvas is a native terminal and agent launcher, not an execution sandbox. A local shell or an explicitly granted collaboration controller runs with the host account's real permissions. Share control only with people trusted with that account. Repository visibility does not expose running terminals by itself.

## Security-sensitive behavior

Collaboration approval is separate from transport connectivity. A pending guest stays pending across disconnects, heartbeat expiry and reconnects; a denied guest must obtain a new admission rather than restoring an old approval. Terminal-control requests are checked against the current shared, alive and controllable panel set and bounded before admission.

Browser captures do not write to the focused terminal. They open a reviewable Launch agent draft with its workspace destination. Launch is an explicit user action. Captures use providers supported by the native-argument launcher; unrecognized Windows batch wrappers are rejected instead of reparsing prompt content through a shell. Web content remains untrusted even after transport and quoting checks.

Pinned collaboration HTTP and WebSocket clients use the same exclusive trust store. A supplied invite must have one certificate. HTTP redirects are not followed, so a redirect cannot forward the credential-bearing request body. A certificate in an invite authenticates only that invite's endpoint; obtain invites through a trusted channel.

Configuration files can contain a Linear API token. On Unix, loading/saving configuration repairs the private directory and current/backup file modes to 0700/0600. Non-regular files and links are rejected. New durable writes use exclusive, randomly named temporary files with private Unix creation permissions. On Windows, the inherited ACL of the per-user configuration directory remains the OS access boundary. Do not move credentials into a shared directory or loosen its ACL.

Password verification accepts the application's bounded Argon2id policy before performing memory-hard work. Verification concurrency and session counts are bounded. Password work runs outside the shared broker registry lock, and admission rechecks session existence, invite validity, expiry, policy and participant capacity afterward.

## Verification

The regression suites are in `src/collab/broker_security_tests.rs`, `src/collab/manager_security_tests.rs`, `src/collab/tls_security_tests.rs` and security-named tests in the application, configuration, password and durable-write modules. CI runs them explicitly with `cargo test --lib --locked security_ -- --nocapture`, in addition to the complete base and Unix daemon suites. Extension tests use `node --test extension/tests/*.test.cjs`.

CI jobs declare read-only repository permissions and do not retain checkout credentials. Compilation and tests are sequential within each platform job. A green test result validates its exact commit and covered cases; it is not an exhaustive independent security certification.

## Operational requirements and remaining scope

Repository administrators should require pull requests and successful CI for `master`, and block force-push and deletion using branch protection or rulesets. Workflow files cannot substitute for those server-side controls. Their configuration must be verified separately using an administrative connection.

Keep the configuration directory private, avoid elevated execution, review captures and agent actions before launching, and treat invite codes and diagnostic logs as sensitive. Diagnostics may contain terminal error output; inspect them before sharing. Do not post live tokens, credentials, invites or unredacted terminal history in public issues.

This remediation does not certify the entire Git history, dependencies, deployed proxies, installed-machine ACLs, browser/OS packaging or signed update identity. Dependency-maintenance notices and manual/platform checks remain separate work. No release, installer or signed package is published by this change. Building or deploying a corrected commit is distinct from updating an already installed copy.
