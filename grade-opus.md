# Grade — `impl/opus` against SPEC.md

Graded 2026-07-25 at `37675ca`. Weighted by the spec, not by general Rust taste.

**Verdict: strong pass, two real deductions. 0.84.** The three primitives work — I ran
them, not read about them. The hard parts the spec singles out (approve-before-anything,
no baked secret, ssh as a real composition with a first connect under
`StrictHostKeyChecking=yes`, a canary that dials its own NodeId from a fresh key) are done
properly and are the pieces worth merging. Two non-negotiables are not met: **the musl
agent does not run** (property 4), and **a relayed session can be reported direct for up to
a probe interval** (property 5). One PROOF claim is outright false — the "hermetic, nothing
leaves this machine" demo talks to n0's pkarr server on every run; I watched the socket.

## Method

Everything below marked *reproduced* I ran myself in a fresh clone at
`~/.cache/botq-wt/1702`, nixpkgs from the repo's own `shell.nix`. Three review sub-agents
covered proto/state/agent, net/controller, and ssh/demo, each cited to `file:line`;
findings I could test, I tested. Applied the code-grade skill's discipline (grade the code
as it is; a comment naming a flaw does not excuse the flaw) but did not write the ledger —
egdod is a one-off competition branch, not a tracked repo, and ledger rows keyed to a
branch that is about to be merged away would be phantoms.

- `cargo test` — **15/15 green, reproduced**, identical to PROOF §2.
- `EGDOD_DEMO_RELAY=1 ./demo.sh` — **completed, exit 0, reproduced end to end** including
  the relay phase. Every step of PROOF §3 reappeared with fresh keys/ports/digests.
- `nix-build musl.nix` — **succeeded**; static PIE, no `INTERP` segment, `--version` runs.
- Independent probes: `env -i` agent (environ 0 bytes, exec by absolute path, connected);
  a `--no-relay` controller's outbound sockets; the musl agent against a live controller.

## The ten questions

**1. Do the three primitives work?** Yes — reproduced, not believed.
`exec`: `to-stdout`/`to-stderr` landed in separate files, exit 7 propagated
(`demo-run.log` step 7; asserted by `demo.sh:136-142`, which also checks each stream is
free of the other's line). `copy`: 64 MiB up and back, three identical sha256s, mode 640 at
source/target/return trip, agent peak RSS 46 MB — i.e. it streams (`proto.rs:209-226`,
`259-277`, 64 KiB chunks, incremental digest). `forward`: a controller-side listener
carried the target's real sshd banner. `exec as root`: a second agent under `sudo` reported
`id -u` = 0 and read `/etc/shadow`. All four proved live.

**2. Is an unapproved agent given nothing, and does approval survive restart?** Yes, and
the gate is structural rather than procedural. Approval is the first statement of the
connection handler, keyed on `conn.remote_id()` — the key QUIC already authenticated, never
a message field — and an unapproved peer gets a `CONNECTION_CLOSE` and is never inserted
into the session map that all routing draws from (`controller.rs:229-249`, `404-418`): two
independent reasons a command cannot reach it. Unreadable approved-list ⇒ deny
(`controller.rs:233`). Approval is a re-read-per-connection file, so `approve` works with
`serve` down (`state.rs:146-170`); demo step 11 killed the controller, restarted it, and the
agent was served with no second approval — reproduced.

**3. Secret material that would be baked into an image?** None. Grep finds no key, token or
salt; the agent's flags are a public NodeId plus addressing; the keypair is generated on
first run from `getrandom` and written `create_new` + 0600 + fsync + rename
(`state.rs:285-347`). A stolen stick is worth exactly the ability to knock. **Full marks.**

**4. Anything needing a screen, keyboard, or human at the target?** No. The agent prints
its pubkey to stdout and dials; approval happens on the controller and is scriptable.
`env -i` agent verified: `/proc/<pid>/environ` 0 bytes, connected, exec'd. **Full marks.**

**5. Static binary, no distro userland?** **This is the first deduction — it does not
work.** `nix-build musl.nix` produces a 26 MB static-pie binary with no `INTERP`, and
`env -i ./egdod --version` runs. Point it at a live controller and it aborts (exit 134),
reproduced verbatim from PROOF §5:

```
thread 'tokio-rt-worker' panicked at noq-udp-1.1.0/src/cmsg/mod.rs:81:5:
assertion failed: align_of::<T>() <= align_of::<C>()
```

Upstream: iroh's UDP layer decodes `SCM_TIMESTAMPNS` as an 8-byte-aligned `timespec` while
musl's `cmsghdr` is 4-byte aligned. The root cause is correct and honestly disclosed. The
author declined to vendor a patch, calling that trade worse than reporting it. **I disagree
on the weight**: property 4 is non-negotiable and the fix is a `[patch.crates-io]` entry
against a fork with one arm skipped. The spec's "single static binary" is delivered as a
build, not as a working agent, and that is the gap between this and a complete v0. Away
from musl the no-userland claim holds: no shell (argv exec'd directly), no dbus, no `/etc`,
no env vars, TLS via bundled roots. DNS is documented honestly, including that with no
`resolv.conf` iroh falls back to 8.8.8.8 — a bare target on defaults talks to a third party.

**6. Does it truthfully report direct-local / direct-LAN / relayed?** **Second deduction —
mostly yes, with a real window where it lies.** No code path can turn a relay address into
a direct label: `classify` maps `TransportAddr::Relay ⇒ Relayed` and only the *selected*
path is reported, with an `Unknown` for anything unclassifiable rather than rounding up to
"direct" (`net.rs:109-160`). That is the right design and the reasoning is written down.
But `status.json`'s session labels are refreshed only at the top of each canary tick
(`controller.rs:317`), so a session that starts direct and **falls back to the relay** keeps
its `direct-lan`/`direct-wan` label for up to `probe_interval + probe_timeout` (~80 s by
default, **unbounded** if an operator raises `--probe-interval`). The per-command line
(`controller.rs:454`) is sampled once and never corrected for a long-lived `forward`. The
author saw the *benign* direction of this (PROOF §6 records `exec` printing `direct-lan`
while the snapshot said `relayed` — I reproduced exactly that in step 15) and the comment
at `controller.rs:314-316` names only the upgrade case. The spec's sentence is about the
other direction. iroh exposes `Connection::paths_stream()`; driving `publish_sessions` off
it closes this. Nits: no `to_canonical()` (`::ffff:192.168.1.7` ⇒ `direct-wan`),
`0.0.0.0`/`::` ⇒ `direct-lan` where `unknown` is correct, CGNAT ⇒ `direct-wan`.

**7. Is ssh a thin composition, and does the first connect survive strict checking?** Yes
to both, and this is the best-executed requirement in the branch. `proto.rs` still has
exactly four `Request` variants — the recipe adds no wire message and uses the same
unix-socket route every CLI command uses: `pull` authorized_keys, `push` the merged file,
`exec` ssh-keygen, `pull` the host key, `exec` sshd, `forward` to its port. Nothing it does
is unreachable from the public CLI. The host key arrives over the egdod channel, never over
ssh; `known_hosts` is written to the state dir (the user's `~/.ssh/known_hosts` is never
read or written) with correct `[host]:port` bracketing for non-22. The connect runs with
`StrictHostKeyChecking=yes BatchMode=yes IdentitiesOnly=yes` against a fresh state dir and
an ephemeral port — so it is genuinely a first connect — and printed `bothouse`. Reproduced.
No path degrades to TOFU; every failure mode fails closed. `ensure_sshd` even reads the
banner *through the forward* rather than trusting a local TCP connect (`ssh.rs:183-207`) —
the trap most implementations fall into.

**8. Can the controller detect that it is undialable?** Partly, and the *reaction* is
worse than the detection. The canary binds a throwaway endpoint with a **fresh random key**
and dials the controller's own NodeId on a separate ALPN — so it evades iroh's self-connect
short-circuit and is a real dial (`net.rs:200-233`). In `Discovery` mode it dials a bare
`EndpointId` and is forced through publish → n0 DNS resolve → dial; step 15 shows that
working for real. Publishing `probe_mode` in `status` so a reader knows what the green light
is worth is the most honest thing in the codebase. Four gaps:
- The dialer is on the same host. A controller behind a firewall dropping inbound UDP still
  reports `dialable: true`. The log line "a stranger can dial us" overclaims — the stranger
  shares your routing table. With `--no-relay` the probe degrades to a loopback ping that
  cannot fail while the process lives (disclosed in PROOF §4 for the hermetic steps).
- **The remedy destroys working sessions.** After `--restart-after` failures the endpoint is
  torn down and `sessions` cleared (`controller.rs:175-190`), with no backoff and no check
  for live agents — and a connected agent is the strongest possible evidence of
  dialability. A phone controller whose discovery path is blocked while agents reach it via
  `--direct` will drop every session, abort any in-flight `push`, and kill every `ssh`,
  every couple of minutes, forever.
- `net::bind` inside `probe` is not timeout-wrapped (`net.rs:211`) while the dial is. If it
  hangs, the canary loop stops, `dialable` stays `true` and `updated_unix` freezes — the
  exact looks-healthy-while-invisible state the requirement exists to prevent. `status`
  never compares `updated_unix` to now, and `status.json` omits `probe_interval`, so a
  program-driven controller cannot compute freshness either.
- A local failure (bind error, resolver hiccup) is reported as "a target booting now would
  not find us".

**9. Are PROOF.md's proved/inferred labels honest?** Overwhelmingly yes — this is the
strongest document of its kind I have graded, and the labels survived spot-checks. I
re-ran four "proved" claims (cargo test, the whole demo, the `env -i` agent, the musl
failure) and all four reproduced. §5 volunteering a broken deliverable with an upstream
file:line, and §4's 12b bullet volunteering that the step proves the *reaction* and not the
detection, are the marks of a real split rather than a decorative one. Five exceptions,
worst first:

- **"hermetic: --no-relay, so nothing leaves this machine" is false** (`demo.sh:57`, echoed
  in PROOF §3). `controller.rs:124` passes `Lookup::Both` unconditionally, independent of
  the relay setting, which attaches `PkarrPublisher::n0_dns()`. **Verified empirically**: a
  `--no-relay` controller held `ESTAB … 49.13.207.244:443` — `dns.iroh.link` — within 8
  seconds of start. Every demo run resolves and announces the controller's public key to a
  third party. The address filter keeps IPs out of the record, so this is a NodeId-existence
  leak plus unannounced network, not an address leak; the claim is still untrue, and an
  unlabelled false statement is precisely what the spec grades as a defect. One-line fix:
  `Lookup::None` when the relay is disabled.
- **Step 5's four `Error: no agent is connected` lines are exec, exec, push, pull** — the
  forward refusal goes to a file that is never printed (`demo.sh:110`). PROOF §4 maps the
  four lines onto four primitives, and PROOF §3 claims only step 14 is filtered. The
  property does hold (the demo separately asserts no file appeared and the socket carried
  nothing); the transcript's accounting of it does not.
- **`forward` is listed as PROVED but the demo asserts nothing**: `head -1` on an empty
  stream exits 0 (`demo.sh:195-197`), so a regression that carries zero bytes passes
  silently. The banner did print in both my run and the author's, so the observation is
  real — it is the *proof* that is not.
- **"exec as root"** leads with `uid seen by exec: 0`, which is printed inside a command
  substitution and never compared (`demo.sh:218`). The next line does assert it, so the
  property is caught — by a different line than the one quoted.
- **"it kept redialling through the whole pending period"** rests on `attempts=1`. It
  redialled at least twice; "the whole period" is an inference sitting in the PROVED
  section. Also: §2's `cargo test` transcript is pasted from a different worktree
  (`…/1700/repo`) — it reproduces here, so provenance wobble, not fabrication.

**10. Any evidence of peeking?** None found, and the strongest evidence is structural: the
competing implementation **was never pushed** — the remote carries only `main` and
`impl/opus` — so there was no branch to fetch. The six commits are a coherent 3-hour
narrative (scaffold → implementation → review pass → demo → PROOF → second review round)
by one author, and the repo's own reflog is my clone's. The only channel that ever existed
was a sibling worker's local worktree on this box, since GC'd; nothing in the tree suggests
it was read. I did not read the competing branch — it does not exist to read.

## Defects, ranked

| # | Sev | Defect | Evidence |
|---|-----|--------|----------|
| 1 | High | musl agent aborts on the first datagram; property 4 unmet | reproduced, exit 134, `noq-udp cmsg/mod.rs:81` |
| 2 | High | Canary failure tears down every live session, no backoff, forever | `controller.rs:175-190`, `349-351` |
| 3 | High | Agent exits permanently if it cannot write its key file — as PID 1 the staging name is `agent.key.tmp.1`, so a power cut during first-boot key creation bricks the target on every subsequent boot (`AlreadyExists` ⇒ `Err` ⇒ exit). Violates "the agent never gives up" | `agent.rs:54`, `state.rs:337-356` |
| 4 | Med-High | Relayed session reported direct for a probe interval (unbounded via flag) | `controller.rs:282-298`, `317` |
| 5 | Med-High | "hermetic / nothing leaves this machine" false; NodeId published to n0 on every run | verified socket; `controller.rs:124` |
| 6 | Med | Concurrent unapproved dials corrupt `pending.json` (pid-only temp name, concurrent tasks within one process) — permanently breaks `pending`, and any stranger holding the public NodeId can force it | `state.rs:352-364`, `controller.rs:238` |
| 7 | Med | exec output silently truncated after a 2 s drain cap, still reports exit 0 | `agent.rs:264-275` |
| 8 | Med | `dialable: true` survives a wedged canary — unbounded `bind`, no age check, `probe_interval` absent from status.json | `net.rs:211`, `controller.rs:705-733` |
| 9 | Med | Unrate-limited blocking fs I/O per unapproved dial (read approved, rewrite 256-entry pending.json) on the async runtime — flash-write amplification on the phone target the spec cares about | `state.rs:196-222` |
| 10 | Med | `--direct` silently disables relay fallback, not just DNS; a fleet shipped that way cannot reconnect after the controller's address changes | `agent.rs:88-92`, help text at `main.rs:50` |
| 11 | Low-Med | Parallel `approve` silently loses approvals (read-modify-write, not `O_APPEND`) | `state.rs:173-186` |
| 12 | Low-Med | `pull` applies an agent-chosen mode including setuid to a controller-side file | `proto.rs:296`, disclosed §10 |
| 13 | Low | Transient control-socket accept error kills the controller (`?` where `forward_listener` correctly warns-and-continues) | `controller.rs:361` |
| 14 | Low | `writer_alive` is a pid check — post-reboot pid reuse makes a dead controller read healthy, though `UnixStream::connect` is already used elsewhere and is exact | `state.rs:63-65` |
| 15 | Low | Two `serve` starts race the socket ⇒ two endpoints on one NodeId | `controller.rs:80-88` |
| 16 | Low | `no agent is connected` conflates unapproved / disconnected / reconnecting | `controller.rs:413` |
| 17 | Low | `direct_addrs` frozen at bind while iroh's address set is live | `controller.rs:134` |
| 18 | Low | `default_path()` falls back to a **relative** `./.local/state/egdod` with no `HOME`/`XDG_STATE_HOME` | `state.rs:80-86` |
| 19 | Low | Classification: no `to_canonical()`, `0.0.0.0`/`::` ⇒ `direct-lan`, CGNAT ⇒ `direct-wan` | `net.rs:119-136` |
| 20 | Low | Failed transfer leaves created parent directories behind, against the comment's "leaves the target untouched" | `proto.rs:248-253` |

Overclaiming comments worth correcting on merge: `net.rs:150-151` ("`None` only while no
path is open at all" — iroh clears `selected` with paths still open, so the user-visible
"no open path" can be false), `net.rs:148-149` (selected is an intent flag, not a
measurement), `controller.rs:54-55` ("not a cached one" — session paths *are* cached for a
probe interval, contradicted by the canary's own comment 260 lines later), `ssh.rs:46-48`
(per-pid scratch does not make `known_hosts` concurrency-safe — it is one shared file,
truncated per run), and PROOF.md:382's dangling cross-reference to a §8 note that isn't there.

Test quality is the soft spot behind a green 15/15. Names that overclaim:
`missing_and_unreadable_are_distinct_replies` asserts two postcard encodings differ (it
tests serde; the ENOENT⇒`Missing` decision at `agent.rs:180-183` is never executed),
`probe_of_a_nonexistent_endpoint_fails` discards the error so any failure passes,
`relay_addr_is_never_reported_as_direct` would still pass if `is_selected()` were dropped
and every relay-plus-candidate session reported "direct", and
`approval_is_by_pubkey_and_survives_restart` reopens a directory rather than restarting
anything — the actual gate has zero unit coverage. The copy tests
(`corrupt_transfer_leaves_no_destination`, `truncated_transfer_is_rejected`,
`copy_verifies_digest_and_preserves_mode`) are the honest ones and are genuinely good.

## Better than spec-minimal

- **`create_part_file`** (`proto.rs:315-336`): `O_EXCL`, random name, 0600, staged beside
  the destination, verified then renamed. A root agent writing a predictable staging path
  in a user-writable directory is a write-anywhere primitive via symlink; most
  implementations ship that bug. This one names it and closes it.
- **`PullStart::Missing` as a distinct wire variant**: the read-modify-write hazard (an
  `authorized_keys` erased because the read failed) is designed out at the type level
  rather than handled at the call site.
- **`ProbeMode` in the status output.** Publishing *how much the green light is worth* —
  loopback ping vs real publish→resolve→dial — is a level of honesty the spec did not ask
  for and the integrator should not lose.
- **`PathKind::Unknown` + selected-path-only reporting**: the failure direction is chosen
  correctly (never round up to direct), and the wildcard arm is mandatory given
  `TransportAddr` is `#[non_exhaustive]`.
- **Session-slot invariant keyed on `(pubkey, stable_id)`** (`controller.rs:261-277`): a
  redial that beat the disconnect watcher cannot evict the live session.
- **Asymmetric `OnUnusable`**: the controller refuses to replace a corrupt key (it has an
  operator, and its key is the NodeId in every image); the agent replaces one (a screenless
  target must not brick). That is thinking about the deployment, not the code.
- **PROOF.md itself.** §5 reporting a broken deliverable with an upstream file:line, §7
  listing the untested defaults *including* that the bare spec-shaped
  `egdod controller ssh <agent>` was never run. The five exceptions above are exceptions to
  a real discipline.

## For the integrator

**Take, close to as-is:** `proto.rs` in full (the codec, the staging/verify/rename copy,
the `Missing` variant, `MAX_MSG`); `state.rs`'s approval model — a file re-read per
connection, fail-closed, `approve` needing no running controller; the `controller.rs`
approval gate and the session-slot invariant; `ssh.rs` whole (the composition, the
bracketed `known_hosts`, the banner-through-the-forward readiness check); `net.rs`'s
`PathKind`/`classify`/`is_selected` and the fresh-key separate-ALPN canary; the
serve-as-dumb-byte-proxy architecture, which is what keeps the protocol at one
implementation per side and ssh honest; `demo.sh` steps 7, 8, 9 and 12b as assertion
templates; PROOF.md's §4/§5/§6/§7 structure.

**Fix before merging:** #1-#5 are all cheap relative to their severity — a
`[patch.crates-io]` fork for musl, a "don't rebuild while sessions are live" guard plus
backoff, retry-with-backoff (never exit) around agent key acquisition, `paths_stream()`
driving `publish_sessions`, and `Lookup::None` when the relay is off. #6 and #9 are one
mutex and one rate limit.

**Discard:** the `head -1` forward check and the `uid` echo in `demo.sh` (replace with
assertions); `probe_of_a_nonexistent_endpoint_fails` and
`missing_and_unreadable_are_distinct_replies` as written; the five overclaiming comments;
`writer_alive`'s pid check in favour of the socket connect the code already uses elsewhere.

**Take from the other branch instead, if it has them:** an external reachability check
(any canary that dials from the same host cannot answer the question the spec asks), and
any live-path test that stands two endpoints up and asserts the reported `PathKind` —
this branch has no test between `Connection` and a label.

## Scores

| Requirement | Grade |
|---|---|
| Three primitives work | Perfect |
| Approve-before-anything, survives restart | Perfect |
| No secret in the image | Perfect |
| No screen/keyboard/human at target | Perfect |
| ssh as a thin composition, strict first connect | Perfect |
| Controller is portable, state under one dir, `--json` | Good |
| Command surface matches the spec | Good |
| Agent never gives up | Good (Work on the key-file brick) |
| PROOF honesty | Good (Work on "hermetic" and step 5) |
| Says which path you got | Work |
| Controller knows it is undialable | Work |
| Static musl binary, no distro userland | Work |
| Test quality behind the green run | Work |
