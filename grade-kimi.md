# grade-kimi — `impl/kimi` against SPEC.md

Graded 2026-07-25 on bothouse (x86_64 NixOS, iroh 1.0.3) at commit `0fab9e2`.
Method: read all 2,641 lines, then re-ran everything rather than believing
`PROOF.md`. `cargo test`, `demo.sh` end to end, plus targeted adversarial probes
(bare-chroot agent, undialable controller, approval gate against all three
primitives, concurrent copy). Every claim below labelled **[ran it]** was
observed in this grading pass; **[read it]** is code inspection only.

**Verdict: strong architecture, honest documentation, two blocking defects.**
One spec property is unmet and misreported as proved; one primitive silently
corrupts data under concurrency. Neither is deep — both are contained, local
fixes — but as it stands this is not a passing v0. On most other axes it is the
better base to build on.

---

## Scorecard

| Spec item | Verdict |
|---|---|
| 1. No secret material in the image | **Pass** [ran it] |
| 2. No screen/keyboard/human at the target | **Pass** [ran it] |
| 3. Approve-before-anything, scriptable, survives restart | **Pass** [ran it] |
| 4. No distro, no userland — static musl binary | **FAIL** [ran it] |
| 5. Say which path you got | **Pass**, one latent caveat [ran it] |
| 6. Controller portable, all state under `--state-dir`, `--json` | **Pass** by design; aarch64 untested |
| 7. The agent never gives up | **Pass** [ran it] |
| `exec` | **Works** [ran it] |
| `copy` | **Works single-transfer; corrupts under concurrency** [ran it] |
| `forward` | **Works** [ran it] |
| ssh as a thin composition, strict first connect | **Pass**, and done well [ran it] |
| Controller knows when it is undialable | **Pass**, done well [ran it] |
| `PROOF.md` proved-vs-inferred honesty | **Mostly honest; one false "proved"** |
| No peeking at the other branch | **No evidence of peeking** |

---

## Blocking defect 1 — the static musl binary does not run

Spec property 4 is non-negotiable: *"The agent must run as a single static
binary (musl)."* It does not.

Built exactly as `PROOF.md` prescribes
(`env -u RUSTC_WRAPPER cargo build --release --target x86_64-unknown-linux-musl`).
The artifact is genuinely static — `file` reports `static-pie linked`, `ldd`
reports `statically linked`. It then **aborts on the first QUIC datagram**:

```
thread 'tokio-rt-worker' panicked at noq-udp-1.1.0/src/cmsg/mod.rs:81:5:
assertion failed: align_of::<T>() <= align_of::<C>()
thread 'tokio-rt-worker' panicked at noq-1.1.0/src/endpoint.rs:495:48:
called `Result::unwrap()` on an `Err` value: PoisonError { .. }
panic in a destructor during cleanup
thread caused non-unwinding panic. aborting.
```

Reproduced 3/3 in every environment: as an unprivileged user on the full host
filesystem, as root on the full host filesystem, and inside a chroot whose
entire contents were the one binary. The controller never saw the agent knock.
`controller init` works (no networking); `controller serve` survives only until
a datagram arrives. The cause is a musl/glibc `cmsghdr` alignment difference in
iroh's vendored `quinn-udp` fork — upstream, not the author's code — but the
deliverable is the binary, and the binary aborts.

This was never caught because **`demo.sh` never executes the musl binary.** Line
208 runs `file` on it and asserts the string `statically linked`. One execution
would have found this.

## Blocking defect 2 — `copy` verifies the wrong bytes, and concurrent copies corrupt

Two parts, one root cause.

**(a) The temp path collides.** Both directions name the staging file
`dest.with_extension("egdod-part")` (`agent.rs:312`, `cmd.rs:169`).
`Path::with_extension` *replaces* the extension, so `x.bin` and `x.txt` both
stage through `x.egdod-part`.

**(b) The sha256 trailer verifies the stream, not the file.** `stream_hashed`
(`lib.rs:211`) hashes the bytes as they pass through and the trailer is compared
against that (`agent.rs:326`). Nothing ever hashes what actually landed on disk.

Together they produce silent corruption **reported as success**. Two concurrent
pushes to `/tmp/egdod-dst/x.bin` and `/tmp/egdod-dst/x.txt`, 4,000,000 random
bytes each:

```
push rc: bin=1 txt=0            <- the .txt push reported SUCCESS
landed size 4000000
per-256KiB-chunk provenance: ?? ?? bin bin ?? bin bin bin bin bin bin bin bin bin bin bin
from bin: 13 | from txt: 0 | neither: 3
identical to either whole source? False False
```

`x.txt` is 13 chunks of the *other* file plus 3 chunks of interleaved garbage,
and zero chunks of its own source. Both trailer checks passed — correctly, since
each stream did deliver its own bytes intact. The winner of the rename race
exits 0; the loser gets `ENOENT`. The spec asks `copy` to *"verify integrity"*;
this verifies an invariant that does not imply it.

Not exotic: pushing a rootfs and its detached signature concurrently
(`root.img` / `root.sig`) hits it. Fix is small — a unique temp suffix, and
verify the hash of what landed.

*(A related but non-blocking wart: a large push to an uncreatable path surfaces
`Broken pipe (os error 32)` instead of the agent's real error. A small push to
the same path correctly reports `creating /tmp/.../y.egdod-part: No such file or
directory`. The useful message is lost exactly when the transfer was expensive.)*

---

## What I verified working

**The three primitives are real, not merely present.** [ran it]

- `exec` — `sh -c 'echo to-stdout; echo to-stderr >&2; exit 3'` returned exit 3
  with stdout and stderr on separate frames and no bleed. Spawn failure returns
  127, distinguishable from the program's own codes; signal deaths are encoded
  negative and reassembled controller-side (`main.rs:230`). The agent runs as
  root under `sudo`, so this is root exec on the target.
- `copy` — 64 MiB pushed, sha256 verified *on the target* by a remote
  `sha256sum`, mode 751 preserved, pulled back byte-identical. Streamed in
  256 KiB chunks with no whole-file buffering anywhere, so larger-than-RAM is
  sound by construction (largest actually moved: 64 MiB). Subject to defect 2
  above.
- `forward` — real `SSH-2.0` banner bytes arrived through a local port bridged
  to the target's sshd; the integration test additionally round-trips a TCP echo.

**The approval gate genuinely gives an unapproved agent nothing.** [ran it]
`serve` records the pubkey to `pending/` and closes the connection before any
registration (`serve.rs:57-66`), so there is no route from the CLI to the agent
at all. I attacked it with all three primitives, not just `exec` as `demo.sh`
does — every one refused (`egdod forward: connection ended: controller: agent
not connected`). The agent redials on backoff and is re-checked each time.
Approval is a file in `approved/`, so restart survival is by construction; I
also observed it live, reconnecting across a controller restart with no
re-approval. `approve` works before an agent's first dial, so scripted
pre-provisioning works.

Two caveats worth an integrator's attention, neither a spec violation:

- An unapproved knock still causes an unauthenticated disk write in `pending/`,
  one file per distinct pubkey, with no cap or rate limit. Anyone who learns the
  public NodeId can grow that directory without bound. "Given nothing" is true
  of capability, not quite of resources.
- There is no revocation: deleting an `approved/` file does not drop a live
  session. The spec does not ask for it.

**No secret material is baked into an image, and nothing needs a human at the
target.** [ran it] The agent takes only `--controller <nodeid>` — public — and
generates its own keypair on first run into `--state-dir` (`agent.rs:30`). I
confirmed a fresh pubkey per state dir across every probe. Approval happens on
the controller, by public key, scriptable via `pending --json` + `approve`.

**The agent needs nothing from a distro that the code doesn't declare** — the
*design* is right even though the musl artifact is broken. [read it + ran it]
No dbus, no NetworkManager, no shell, no `/etc` assumption; a `--relay` URL or
`--direct` hint removes the DNS dependency, and `--relay` additionally calls
`clear_address_lookup()` (`agent.rs:71`) so discovery is genuinely skipped. Note
that `demo.sh`'s "initramfs conditions" phase masks `/etc/resolv.conf` inside a
netns but leaves `/etc`, `/bin` and the glibc loader in place — it is a
no-**DNS** test, not a no-**userland** test. My chroot test is the latter, and it
is defect 1.

**Path reporting is honest.** [ran it] Labels come from the *selected* path
(`lib.rs:239-246`) and are printed on both ends at session start and again on
every change. I saw a forced-relay session correctly report
`relayed (relay https://usw1-1.relay.n0.iroh.link./)`, and I saw a genuine
upgrade line, `path changed: relayed -> direct-LAN (172.18.0.1:33639)`. No
relayed connection ever passed as direct. The extra fourth label
`direct-public`, so a direct path over a public IP is not miscalled "LAN", is
the right instinct.

One latent caveat [read it]: when *no* path is selected, `classify_path` falls
back to an arbitrary `paths.iter().next()`. If a relay path and an unselected
direct candidate coexist mid-migration, the transient label could read direct
while traffic is relayed. Self-correcting — the watcher re-reports on the next
change — but it is the one line where the spec's forbidden masquerade could
occur, and it should be `"unknown"` rather than a guess.

**The ssh recipe is a thin composition, and the strict first connect is real.**
[ran it] `cmd.rs:263-364` is `exec` (mkdir/chmod, `sshd -t` probe), `pull`+`push`
(merge `authorized_keys`, never clobber), `pull` (host public key), `forward`
(local port → `127.0.0.1:22`), then the *system* `ssh` client. No second
transport, no bespoke crypto, no privileged path — every step is spelled in the
three primitives.

The first connect succeeded under `-o StrictHostKeyChecking=yes -o
BatchMode=yes` and returned `id -u` = 0. I did not take that on faith: I
compared the `known_hosts` entry the recipe wrote against the target's real
`/etc/ssh/ssh_host_ed25519_key.pub` and the key blobs are byte-identical, so the
trust genuinely arrived in band over the authenticated egdod channel rather than
on first-use trust.

I also exercised the merge path that job 1699's run could not have: this host's
`/root/.ssh/authorized_keys` had a pre-existing key when I ran it, and the
recipe preserved it and appended, rather than clobbering.

**The controller knows when it is undialable — the best thing here.** [ran it]
This is the spec's sharpest named hazard and it is handled properly. A separate
in-process endpoint dials the controller's *own* NodeId with no address hints,
so it exercises exactly what a booting target exercises, on its own
`CANARY_ALPN` so a self-dial can never be mistaken for an unapproved agent and
poison `pending` (`serve.rs:22`, `serve.rs:328-384`). It runs immediately at
boot — where the publish race actually lives — then every 30s, and writes
`reachability.json`.

Both directions verified. Healthy:

```json
"reachability": { "ok": true, "path": "relayed (relay https://usw1-1.relay.n0.iroh.link./)" }
```

Undialable (controller started `--no-relay`), a loud console warning plus:

```
$ egdod controller status --state-dir ./m-ctl
dialable: NO — self-dial by NodeId failed: No addressing information available: ...
approved agents: 0; pending: 0
$ echo $?
2
```

Exposed as state, machine-readable, and a nonzero exit code a supervisor can
act on. A controller here cannot confuse "no targets today" with "nobody can
find me".

**`cargo test` is green and reproduces exactly** [ran it] — 8 unit + 1
integration, matching `PROOF.md`'s pasted output. The integration test is
substantive rather than decorative: real loopback QUIC, the refusal-before-
approval assertion, exec stream separation, 127 on spawn failure, a 5 MiB
round trip with mode, a forwarded TCP echo, and an assertion that the path label
starts with `direct-`.

---

## Is `PROOF.md` honest?

**Largely yes — unusually so — with one false "proved".** The `INFERRED` section
is candid and, as far as I can check, correct: it volunteers that throughput was
never measured, that `--relay` with an IP literal is *suspected not to work*
against n0's public relays, that the no-paths watchdog's trigger was reasoned
rather than reproduced, and that the multi-agent registry race has no dedicated
test. That is the behaviour the contract asks for.

I re-ran two `PROVED` claims in full. Both held exactly: the `cargo test` output,
and the entire demo transcript — including approval-survives-restart and the
`relayed -> direct-LAN` upgrade line.

Two claims do not survive:

1. **False.** *"No-DNS/no-userland operation: … the agent dialed, was approved,
   and execd — nothing but a kernel, one static binary, and UDP."* The phase in
   question runs `$BIN` = `target/debug/egdod`, which is
   `dynamically linked, interpreter …/glibc-2.42-47/lib/ld-linux-x86-64.so.2`,
   on a full NixOS root. It was not one static binary, and the static binary
   cannot do what the sentence claims. This is the most serious documentation
   defect here: the one spec property that fails is the one asserted as proved.

2. **Unsupportable as an observation.** *"authorized_keys merged via pull+push
   (never clobbered — checked against this host's pre-existing file)."* There
   was no pre-existing file: `demo.sh` wipes its state dir and generates a fresh
   ssh key each run, so each run appends one key, and this host had exactly one
   before I started. The merge branch cannot have been exercised. The *behaviour*
   is nonetheless correct — I re-ran it with a pre-existing key present and it
   merged — so this is a mislabel, not a false capability claim.

Also worth noting: `PROOF.md` explicitly bases the "static musl binary present"
line on `file` output alone and never claims to have run it. Read strictly, it
does not lie about defect 1 — it just never checked, and then the no-userland
bullet overreached.

## Did the author peek at the other branch?

**No evidence, and positive evidence against.** [ran it — metadata only]

The reflog contains only my own clone. Comparing the two branches by *blob hash*
(no content read): the only byte-identical files are `LICENSE-MIT`,
`LICENSE-APACHE` and `SPEC.md`, all inherited from the shared scaffold commit
`df6488d`. Not one source file matches. The decompositions are unrelated —
`impl/kimi` is 5 source files with the protocol in `lib.rs`; `impl/opus` is 9,
with `proto.rs`/`net.rs`/`pipe.rs`/`ssh.rs`/`state.rs` split out.

The opportunity existed: `impl/opus` finished at 00:27 and `impl/kimi`'s first
implementation commit is 02:40 the same night, so the other branch was on the
remote and visible. The artifacts say it wasn't used. To check the clause I read
only branch names, commit timestamps, commit subjects and tree hashes on
`impl/opus` — never its file contents.

---

## For the integrator

### Take these

1. **The reachability canary** (`serve.rs:328-384`). The single most valuable
   thing on this branch. Verified in both directions. The separate `CANARY_ALPN`
   so a self-dial cannot poison `pending` is a detail that is easy to get wrong
   and is right here.
2. **Approval as plain files** in `approved/`/`pending/`. Restart survival is by
   construction rather than by care; scriptable with `mv` and `echo` as well as
   the CLI; no database, no schema, no migration. `pending` keeps `first_seen`
   across re-knocks and refreshes `last_seen`.
3. **The control-socket bridge** (`serve.rs:191-242`). `serve` routes and knows
   no protocol; protocol knowledge lives once in `cmd.rs` and once in
   `agent.rs`. One-shot CLI commands drive the long-lived daemon without a
   second transport. Clean seam, and the reason the ssh recipe stayed thin.
4. **Dropping the prior art's capability layer.** QUIC already authenticates the
   peer's public key, so approval can bind that key directly. Same security
   property, fewer moving parts, and correctly argued in `PROOF.md`.
5. **Reporting path *changes*, not just the first label** (`agent.rs:134-146`).
   The spec asks for a label; transitions are strictly more honest, and I
   watched it fire.
6. **`demo.sh` as an assertion harness.** Every phase `fail`s loudly rather than
   printing and continuing. It reproduced on my run without a single edit — that
   is rarer than it should be.
7. **The `direct-public` fourth label.** Refusing to call a public direct path
   "LAN" is the spec's spirit applied where the spec was silent.

### Fix or discard these

1. **The copy temp path and the integrity invariant** (defect 2). Unique
   per-transfer temp name, and verify the hash of what *landed*. Blocking.
2. **The musl story** (defect 1). Either work around the `noq-udp` alignment
   abort or stop claiming static musl works — and make `demo.sh` *run* the
   binary it currently only runs `file` on. Blocking.
3. **`pull_to_memory`** (`cmd.rs:368-390`) is a second implementation of
   `pull`'s protocol client, kept in sync by hand. Exactly the drift hazard.
   Fold to one reader with a pluggable sink.
4. **`demo.sh`'s `pkill -f` patterns.** `pkill -f "egdod agent"` matches any
   process whose argv contains that string — including the shell running the
   grade. It cost me two killed sessions during this pass. Use pidfiles.
5. **Unbounded `pending/`.** Cap it, or rate-limit unknown knocks.
6. **`classify_path`'s no-selected-path fallback** (`lib.rs:241`). Return
   `"unknown"` rather than guessing at the one place the spec forbids guessing.
7. **Error masking on large failed pushes.** Read the agent's status before
   committing to stream the body, or surface the reset reason.
8. Minor: `default_state_dir()` `Box::leak`s once per subcommand field
   (`main.rs:125-134`); the agent's default state dir is the fixed
   `/var/lib/egdod-agent`, which is fine but is the one fixed system path left.

### What this does better than a spec-minimal implementation

A spec-minimal build would satisfy "expose the reachability state" with a log
line; this dials itself over a distinct ALPN and gives you a JSON file plus an
exit code. A spec-minimal build would report a path label once; this reports
every transition. A spec-minimal build would write `authorized_keys`; this pulls
it first and merges. A spec-minimal build would not have noticed that a wedged
session with zero open paths never produces a QUIC close frame, and would sit
there forever instead of redialing (`agent.rs:101-115`). The instinct throughout
is to close the gap between "the code path exists" and "the operator can tell
what happened" — which is precisely the axis the spec cares about.

Which makes the two defects the more frustrating: both would have been caught by
running the artifact rather than inspecting it. The discipline that produced the
canary was not applied to the binary the canary is supposed to protect.

---

## Code-grade clusters

| Cluster | Grade | Note |
|---|---|---|
| correctness | **Work** | Copy's integrity invariant does not imply integrity; concurrent copies corrupt and report success. |
| structure | **Good** | Sharp seams: protocol once in `lib.rs`, routing that knows no protocol, ssh as pure composition. `pull_to_memory` is the one duplicate. |
| interface | **Good** | Command surface matches the spec; `--json` where a program is the caller; `status` exit code is actionable. |
| security | **Good** | Authority is the QUIC-authenticated public key, approval is fail-closed, no secret leaves the target. Docked for the unbounded `pending/` write and the unauthenticated control socket resting solely on directory mode. |
| change | **Good** | Test suite is real and would catch regressions in everything except the two defects found here — neither has a test. |
| writing | **Good** | Comments are why-not-what throughout and none were stale against current code. Docked for the one false "proved". |
| meta | **Good** | Respects the reader's attention; `PROOF.md` is a genuine artifact rather than a brochure. |

**Bottom line for the merge:** take this branch's controller — canary, file-based
approval, the routing bridge — and its ssh recipe essentially as written. Do not
ship its `copy` until the temp-path and hash-target bugs are fixed, and do not
believe any static-musl claim until someone executes the binary.
