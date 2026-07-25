# grade-kimi — `impl/kimi` against SPEC.md

Graded 2026-07-25 on bothouse (x86_64 NixOS, iroh 1.0.3) at commit `0fab9e2`.
Method: read all 2,641 lines, then re-ran everything rather than believing
`PROOF.md` — `cargo test`, `demo.sh` end to end, plus adversarial probes the
demo does not do (agent in a chroot containing one file, undialable controller,
approval gate attacked with all three primitives, concurrent copy, copy of a
procfs file, state-dir relocation). Findings were then fanned out to two
independent reviewers and triaged; where the reviewers disagreed I settled it by
experiment.

Every claim marked **[ran it]** was observed in this pass. **[read it]** is code
inspection only. Nothing below is repeated from `PROOF.md` on trust.

**Verdict: strong architecture, unusually honest documentation, three blocking
defects.** One spec property is unmet and asserted as proved; `copy`'s integrity
guarantee verifies the wrong thing and I broke it two different ways; the ssh
recipe can destroy a target's existing root keys. None is deep — all are
contained, local fixes — but as it stands this is not a passing v0. On most
other axes it is still the better base to build on.

---

## Scorecard

| Spec item | Verdict |
|---|---|
| 1. No secret material in the image | **Pass** [ran it] |
| 2. No screen/keyboard/human at the target | **Pass** [ran it] |
| 3. Approve-before-anything, scriptable, survives restart | **Pass**, two caveats [ran it] |
| 4. No distro, no userland — static musl binary | **FAIL** [ran it] |
| 5. Say which path you got | **Pass**, one latent caveat [ran it] |
| 6. Controller portable, all state under `--state-dir`, `--json` | **Pass** by design; relocation drops the state dir's 0700 [ran it] |
| 7. The agent never gives up | **Pass** [ran it] |
| `exec` | **Works** [ran it] |
| `copy` | **Works single-transfer; integrity check verifies the wrong bytes** [ran it] |
| `forward` | **Works**; spec's documented argument form doesn't [ran it] |
| ssh as a thin composition, strict first connect | **Pass**, and done well — but it can clobber authorized_keys [ran it] |
| Controller knows when it is undialable | **Pass for the case that matters; cries wolf in `--no-relay`** [ran it] |
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

Reproduced 3/3 in every environment: unprivileged on the full host filesystem,
as root on the full host filesystem, and in a chroot whose entire contents were
the one binary. The controller never saw the agent knock. `controller init`
works (it never touches the network); `controller serve` survives only until a
datagram arrives. The cause is a musl/glibc `cmsghdr` alignment difference in
iroh's vendored `quinn-udp` fork — upstream, not the author's code — but the
deliverable is the binary, and the binary aborts.

Never caught because **`demo.sh` never executes the musl binary.** Line 208 runs
`file` on it and greps for `statically linked`; the phase is also conditional on
the binary existing, so it cannot fail. One execution would have found this.

## Blocking defect 2 — `copy` verifies the bytes that moved, not the bytes that landed

The sha256 trailer is computed by `stream_hashed` (`lib.rs:211`) over data as it
passes through, and compared against the sender's trailer (`agent.rs:326`).
Nothing ever hashes what is on disk, and nothing checks that what was on disk is
what was asked for. The spec asks `copy` to *"verify integrity"*; this verifies
an invariant that does not imply it. Two independent breaks, both [ran it]:

**(a) Concurrent copies corrupt and report success.** Both directions stage
through `dest.with_extension("egdod-part")` (`agent.rs:312`, `cmd.rs:169`), and
`Path::with_extension` *replaces* the extension — so `x.bin` and `x.txt` share
one temp path. Two concurrent pushes of 4,000,000 random bytes each:

```
push rc: bin=1 txt=0            <- the .txt push reported SUCCESS
landed size 4000000
per-256KiB-chunk provenance: ?? ?? bin bin ?? bin bin bin bin bin bin bin bin bin bin bin
from bin: 13 | from txt: 0 | neither: 3
identical to either whole source? False False
```

`x.txt` is 13 chunks of the *other* file plus 3 chunks of interleaving, and zero
chunks of its own source. Both trailer checks passed — correctly, since each
stream did deliver its own bytes intact. The rename race decides who exits 0.
Not exotic: pushing `root.img` and `root.sig` concurrently hits it.

**(b) `pull` silently truncates any file whose stat size lies, and calls it
verified.** `get_inner` trusts `meta.len()` (`agent.rs:371`) for both the
announced length and the read loop:

```
$ egdod controller exec $PK -- sh -c 'stat -c "stat says %s bytes" /proc/version; wc -c < /proc/version'
stat says 0 bytes
146
$ egdod controller pull $PK /proc/version ./v-out.txt
pulled 0 bytes from /proc/version
$ echo $?; wc -c < v-out.txt
0
0
```

A 146-byte file arrives as an empty one, exit 0, hash "verified". Same for any
file appended to mid-transfer: silently cut to its stat-time prefix, trailer
matching. For a tool whose job is pulling `/proc`, `/sys` and logs off a machine
with no OS, this is the wrong failure.

**(c) Same root cause, security flavour** [read it]: the temp path is fully
predictable and `File::create` follows symlinks, as root. An unprivileged local
user on the target pre-creates `/tmp/x.egdod-part` → `/etc/shadow`; the next
push to `/tmp/x.<anything>` writes through it, chmods it to the sender's mode,
and renames. The temp file is also created at the default umask and chmodded
only *after* the whole payload lands (`agent.rs:313-335`), so a 0600-destined
secret is world-readable for the duration.

One fix addresses all three: `mkstemp` in the destination directory, and verify
the hash of what landed rather than of what moved.

## Blocking defect 3 — the ssh recipe can destroy a target's existing root keys

`cmd.rs:297-300`:

```rust
let mut merged = match pull_to_memory(&state_dir, &agent, "/root/.ssh/authorized_keys").await {
    Ok(existing) => String::from_utf8_lossy(&existing).into_owned(),
    Err(_) => String::new(),
};
```

`Err(_)` collapses "the file does not exist yet" — the legitimate first-run
case — with "the pull failed". Any transient stream error, any file over the
16 MiB `pull_to_memory` cap, any hash mismatch yields an empty string, and
`push` at line 313 then overwrites `/root/.ssh/authorized_keys` with egdod's key
alone. Every other root key on that machine is gone, and the comment three lines
above says *"merge, never clobber"*.

The merge itself is correct — I exercised it against a genuinely pre-existing
key and it preserved and appended (see below). It is only the error branch that
is destructive, and it needs to distinguish `NotFound` from everything else.

---

## What I verified working

**The three primitives are real, not merely present.** [ran it]

- `exec` — `sh -c 'echo to-stdout; echo to-stderr >&2; exit 3'` returned exit 3
  with stdout and stderr on separate frames and no bleed. Spawn failure returns
  127, distinguishable from the program's own codes; signal deaths are encoded
  negative and reassembled controller-side (`main.rs:230`). The agent runs as
  root under `sudo`, so this is genuinely root exec on the target.
- `copy` — 64 MiB pushed, sha256 verified *on the target* by a remote
  `sha256sum`, mode 751 preserved, pulled back byte-identical. Streamed in
  256 KiB chunks with no whole-file buffering on the bulk path, so
  larger-than-RAM is sound by construction. Subject to defect 2.
- `forward` — real `SSH-2.0` banner bytes arrived through a local port bridged
  to the target's sshd; the integration test additionally round-trips a TCP echo.

**The approval gate genuinely gives an unapproved agent nothing.** [ran it]
`serve` records the pubkey to `pending/` and closes the connection before any
registration (`serve.rs:57-66`), so no route from the CLI to the agent exists at
all. I attacked it with all three primitives, not just `exec` as `demo.sh` does
— every one refused (`egdod forward: connection ended: controller: agent not
connected`). Approval is a file in `approved/`, so restart survival is by
construction; I also watched it live, reconnecting across a controller restart
with no re-approval. `approve` works before an agent's first dial, so scripted
pre-provisioning works.

Two caveats, neither a spec violation but both an integrator's problem:

- **An unapproved agent knocks about once a second, forever, and each knock
  writes a file.** The controller's "pending approval" close is a *clean* close,
  so `dial_once` returns `Ok(())` and the backoff resets to its 1s floor
  (`agent.rs:47-51`) — the one case that most needs backoff doesn't get it. I
  logged 8 full handshake-plus-rewrite cycles in the few seconds before I
  approved. Each writes `pending/<pubkey>` (`serve.rs:266-279`) with no cap and
  no rate limit, and anyone holding the public NodeId — which is baked into
  every image by design — can mint unlimited keypairs. On the phone-hosted
  controller of spec property 6 that is the whole threat model.
- **No revocation.** Approval is checked once, at registration; `bridge_session`
  (`serve.rs:206-216`) re-checks nothing. Deleting `approved/<pk>` leaves the
  live connection in the registry, so a revoked target keeps root exec until it
  disconnects on its own. The spec doesn't ask for revocation, but anyone
  merging this should know it isn't there.

**No secret material is baked into an image, and nothing needs a human at the
target.** [ran it] The agent takes only `--controller <nodeid>` — public — and
generates its own keypair on first run into `--state-dir` (`agent.rs:30`); I
confirmed a fresh pubkey per state dir across every probe. Approval happens on
the controller, by public key, scriptable via `pending --json` + `approve`.

**The agent needs nothing from a distro that the code doesn't declare** — the
*design* is right even though the musl artifact is broken. [read it + ran it]
No dbus, no NetworkManager, no shell, no `/etc` assumption; a `--relay` URL or
`--direct` hint removes the DNS dependency, and `--relay` additionally calls
`clear_address_lookup()` (`agent.rs:71`) so discovery is genuinely skipped.

Note that `demo.sh`'s "initramfs conditions" phase masks `/etc/resolv.conf`
inside a namespace but leaves `/etc`, `/bin` and the glibc loader in place — and
the block itself uses `bash`, `mount`, `ip`, `grep`, `ls`, `seq`. It is a
no-**DNS** test, not a no-**userland** test. My chroot test is the latter, and it
is defect 1.

**Path reporting is honest.** [ran it] Labels come from the *selected* path
(`lib.rs:239-246`) and print on both ends at session start and on every change.
I saw a forced-relay session correctly report `relayed (relay
https://usw1-1.relay.n0.iroh.link./)`, and a genuine upgrade line, `path
changed: relayed -> direct-LAN (172.18.0.1:33639)`. No relayed connection ever
passed as direct. The extra fourth label `direct-public`, so a direct path over
a public IP isn't miscalled "LAN", is the right instinct.

One latent caveat [read it]: when *no* path is selected, `classify_path` falls
back to an arbitrary `paths.iter().next()`. If a relay path and an unselected
direct candidate coexist mid-migration, the transient label could read direct
while traffic is relayed. Self-correcting via the watcher, but it is the one
line where the spec's forbidden masquerade could occur, and it should return
`"unknown"` rather than guess.

**The ssh recipe is a thin composition, and the strict first connect is real.**
[ran it] `cmd.rs:263-364` is `exec` (mkdir/chmod, `sshd -t` probe), pull+`push`
(merge authorized_keys), pull (host public key), `forward` (local port →
`127.0.0.1:22`), then the *system* `ssh` client. Every byte to or from the
target rides the one dialed-out iroh connection under
`op::EXEC`/`op::PUT`/`op::GET`/`op::FWD`. No second transport, no bespoke
crypto, no privileged side channel. The only local processes spawned are
`ssh-keygen` and `ssh`, both of which the spec's recipe requires.

The first connect succeeded under `-o StrictHostKeyChecking=yes -o
BatchMode=yes` and returned `id -u` = 0. I did not take that on faith: I
compared the `known_hosts` entry the recipe wrote against the target's real
`/etc/ssh/ssh_host_ed25519_key.pub` and the key blobs are **byte-identical**, so
the trust genuinely arrived in band over the authenticated egdod channel rather
than on first use. That is the part of the spec most likely to be faked, and it
isn't.

I also exercised the merge path that job 1699's run could not have: this host's
`/root/.ssh/authorized_keys` had a pre-existing key when I ran it, and the
recipe preserved it and appended.

Two blemishes short of the "thin composition" claim: steps 3 and 6 use
`pull_to_memory` (`cmd.rs:368-390`), a hand-rolled second client of the copy
wire format rather than the published `pull` — so `cmd.rs:252-254`'s "everything
below is spelled in the three primitives" is two-eighths untrue. And the forward
task is spawned fire-and-forget (`cmd.rs:343-345`), so a forward failure reaches
the user as an unexplained refused connection.

**`cargo test` is green and reproduces exactly** [ran it] — 8 unit + 1
integration, matching `PROOF.md`'s pasted output. The integration test is
substantive rather than decorative: real loopback QUIC, refusal-before-approval,
exec stream separation, 127 on spawn failure, a 5 MiB round trip with mode, a
forwarded TCP echo, and an assertion that the path label starts with `direct-`.

## The reachability canary — right idea, and it cries wolf

This is the spec's sharpest named hazard, and the mechanism is the best thing on
the branch. A separate in-process endpoint dials the controller's *own* NodeId
with no address hints, so it exercises exactly what a booting target exercises,
on its own `CANARY_ALPN` so a self-dial can never be mistaken for an unapproved
agent and poison `pending` (`lib.rs:22`, `serve.rs:328-384`). It runs
immediately at boot — where the publish race actually lives — then every 30s,
writes `reachability.json`, and `status` exposes it with an actionable exit code:

```
$ egdod controller status --state-dir ./m-ctl
dialable: NO — self-dial by NodeId failed: No addressing information available: ...
approved agents: 0; pending: 0
$ echo $?
2
```

For the relay/discovery deployment — the one the spec's hazard is about — it is
correct in both directions, and I verified both.

**But it is a guaranteed false alarm in `--no-relay`.** [ran it] It always dials
by bare NodeId with no hints (`serve.rs:368`), while the `--no-relay --direct`
deployment never uses discovery at all. On a controller that was concurrently
serving an approved agent perfectly well — exec working, `approved/` populated —
I counted **27 UNDIALABLE warnings** in one run:

```
egdod serve: WARNING controller appears UNDIALABLE by NodeId 8b9c667a... (targets
booting now cannot find me; check discovery publish / relay)
```

`demo.sh` itself runs the controller `--no-relay` in its no-DNS phase, so the
demo generates these too. A warning that fires permanently in a supported mode
trains the operator to ignore the one warning the spec demanded. The canary
needs to know which discovery mechanisms are actually in play, or say "not
applicable" rather than "undialable". Not listed in `PROOF.md`'s INFERRED or
Not-tested sections.

## Relocating a controller silently drops its 0700

Spec property 6: *"relocating a controller is copying a directory."* Do that and
the state dir loses the permissions that are the control socket's only guard.

`bind()` runs `create_dir_all(state_dir/approved)` first (`serve.rs:132`),
creating the state dir at the default umask. `load_or_create_secret_key` then
chmods the parent to 0700 — but **only in the key-creation branch**
(`lib.rs:64-73`), and discards the result (`let _ =`). When the key already
exists, control takes the `Ok(bytes)` branch at `lib.rs:52` and nothing ever
chmods or checks anything again. Verified [ran it]:

```
after init:            700
after cp -r + chmod:   755      <- what cp -r / tar without -p leaves
after re-running init: 755      <- never re-established
after a pending call:  755
```

This matters because the unix control socket (`serve.rs:171-186`) is
unauthenticated — no `SO_PEERCRED`, no token — and is root-equivalent on every
approved target. `UnixListener::bind` never sets the socket's own mode either,
so it lands at the default umask; under a `002` umask any user in the owner's
group can connect and get root exec on every approved target. `approve()`
likewise writes at the default umask, so a group-writable `approved/` lets a
local user self-approve with `touch`.

`PROOF.md:237-238` states this as settled fact — *"the control socket is the
trust boundary … it lives under the state dir, which `init` creates mode
0700"* — filed under "choices the spec left open" rather than INFERRED. True
only for a state dir created by this program, on the run that created the key.

---

## Is `PROOF.md` honest?

**Largely yes — unusually so — with one false "proved".** The INFERRED section
is candid and, as far as I can check, correct: it volunteers that throughput was
never measured, that `--relay` with an IP literal is *suspected not to work*
against n0's public relays, that the no-paths watchdog's trigger was reasoned
rather than reproduced, and that the multi-agent registry race has no dedicated
test. That is the behaviour the contract asks for, and it is rarer than it
should be.

I re-ran two PROVED claims in full. Both held exactly: the `cargo test` output,
and the whole demo transcript — including approval-survives-restart and the
`relayed -> direct-LAN` upgrade line.

What does not survive:

1. **False.** *"No-DNS/no-userland operation: … the agent dialed, was approved,
   and execd — nothing but a kernel, one static binary, and UDP."* That phase
   runs `$BIN` = `target/debug/egdod`, which is `dynamically linked, interpreter
   …/glibc-2.42-47/lib/ld-linux-x86-64.so.2`, on a full NixOS root, in a shell
   script that uses six other userland binaries. It was not one static binary,
   there was a great deal of userland, and the static binary cannot do what the
   sentence claims. The one spec property that fails is the one asserted as
   proved — the most serious class of defect available here.
2. **Unsupportable as an observation.** *"authorized_keys merged via pull+push
   (never clobbered — checked against this host's pre-existing file)."* There was
   no pre-existing file: `demo.sh` wipes its state dir and generates a fresh ssh
   key each run, so each run appends one key, and this host had exactly one
   before I started. The merge branch cannot have been exercised. The behaviour
   is nonetheless correct — I re-ran it with a pre-existing key and it merged —
   so this is a mislabel, not a false capability claim.
3. **By-construction claim filed under PROVED, and not universally true.**
   *"transfer is chunked, never whole-file in RAM."* Honestly re-listed under
   INFERRED a few lines later, but `pull_to_memory` (`cmd.rs:368-390`) does
   buffer whole files in RAM, and it is what the ssh recipe uses.
4. **Module doc asserts a property the code doesn't have.** `lib.rs:1-3`: *"the
   protocol lives here exactly once so the two sides cannot drift"* — while
   `cmd.rs:4` says the accurate thing, *"Protocol knowledge lives here and in
   agent.rs"*. Every op's layout is hand-encoded on both sides; the GET reply
   header is parsed in two places. `lib.rs:95` also points at "module docs for
   the layout" and no such description exists.
5. **Unrecorded spec deviation.** SPEC:71 specifies `forward <agent>
   <local-port> <remote-addr:port>`; the implementation takes a full bind string
   (`main.rs:98`), so the spec's own documented invocation fails [ran it]:
   `egdod: error: binding local listener 12223: invalid socket address`.
   SPEC:60-61 requires deviations be recorded in `PROOF.md`; several others are,
   this one isn't.
6. **`--help` asserts what PROOF suspects is broken.** `main.rs:22` — "Use only
   this relay (an IP-literal URL needs no DNS)" — while `PROOF.md:204-206` calls
   that exact path untested and *suspected not to work*. The user-facing string
   is more confident than the evidence.

To be fair to the author on defect 1: `PROOF.md` bases the "static musl binary
present" line on `file` output alone and never claims to have executed it. Read
strictly it doesn't lie there — it just never checked, and then the no-userland
bullet overreached.

## Did the author peek at the other branch?

**No evidence, and positive evidence against.** [ran it — metadata only]

The reflog contains only my own clone. Comparing the branches by *blob hash* (no
content read): the only byte-identical files are `LICENSE-MIT`,
`LICENSE-APACHE` and `SPEC.md`, all inherited from the shared scaffold commit
`df6488d`. Not one source file matches. The decompositions are unrelated —
`impl/kimi` is 5 source files with the protocol in `lib.rs`; `impl/opus` is 9,
with `proto.rs`/`net.rs`/`pipe.rs`/`ssh.rs`/`state.rs` split out.

The opportunity existed: `impl/opus`'s last commit is 00:27 and `impl/kimi`'s
first implementation commit is 02:40 the same night, so the other branch was on
the remote and visible. The artifacts say it wasn't used. To check the clause I
read only branch names, commit timestamps, commit subjects and tree hashes on
`impl/opus` — never its file contents.

---

## For the integrator

### Take these

1. **The reachability canary** (`serve.rs:328-384`), with the `--no-relay` false
   alarm fixed. The single most valuable idea on this branch. The separate
   `CANARY_ALPN` so a self-dial cannot poison `pending` is a detail that is easy
   to get wrong and is right here.
2. **Approval as plain files** in `approved/`/`pending/`. Restart survival by
   construction rather than by care; scriptable with `mv` and `echo` as well as
   the CLI; no database, no schema, no migration. `pending` keeps `first_seen`
   across re-knocks and refreshes `last_seen`.
3. **The control-socket bridge** (`serve.rs:191-242`). `serve` routes and knows
   no protocol; protocol lives in `cmd.rs` and `agent.rs`. One-shot CLI commands
   drive the long-lived daemon without a second transport. This seam is the
   reason the ssh recipe stayed thin, and it is the design decision most worth
   carrying over. (Authenticate the socket and fix the mode first.)
4. **Dropping the prior art's capability layer.** QUIC already authenticates the
   peer's public key, so approval can bind that key directly. Same security
   property, fewer moving parts, correctly argued in `PROOF.md`.
5. **Reporting path *changes*, not just the first label** (`agent.rs:134-146`).
   The spec asks for a label; transitions are strictly more honest, and I
   watched it fire.
6. **`demo.sh` as an assertion harness.** Every phase `fail`s loudly rather than
   printing and continuing, and it reproduced on my run without a single edit.
   Rarer than it should be. (Make the musl phase non-optional and make it
   *execute* the binary.)
7. **The `direct-public` fourth label.** Refusing to call a public direct path
   "LAN" is the spec's spirit applied where the spec was silent.
8. **The no-paths watchdog** (`agent.rs:101-115`) and the honesty about why it
   exists — a session with zero open paths never produces a QUIC close frame, so
   without it the agent waits forever instead of redialing.

### Fix or discard these

1. **`copy`'s integrity model** (defect 2). `mkstemp` in the destination
   directory; verify the hash of what landed; stop trusting `meta.len()` for
   procfs and growing files. Blocking, and there is no test for any of it.
2. **The musl story** (defect 1). Work around the `noq-udp` alignment abort or
   stop claiming static musl works — and make `demo.sh` run the binary it
   currently only runs `file` on. Blocking.
3. **The authorized_keys error branch** (defect 3). Distinguish `NotFound` from
   failure. Blocking; it is destructive.
4. **The canary's `--no-relay` false alarm.** Make it aware of which discovery
   mechanisms are in play.
5. **State-dir and socket permissions.** Re-establish and *verify* 0700 on every
   start, not only when creating the key; set the socket's mode explicitly;
   authenticate the control socket with `SO_PEERCRED`.
6. **`pull_to_memory`** (`cmd.rs:368-390`) — a second implementation of `pull`'s
   protocol client, kept in sync by hand. Make `pull` sink-generic and it
   collapses to three lines. Same story for relay-mode selection: `serve.rs:26`
   is `pub` and `agent.rs:36-42` open-codes the identical three branches. One
   implementation per thing.
7. **Hand-encoded wire format on both sides.** `cmd.rs` and `agent.rs` encode
   and decode every op independently, with `op`/`frame` as bare `u8` consts. One
   typed codec both peers call would delete the duplication and the drift, and
   would have made `pull_to_memory` impossible to write.
8. **`exec` never kills the child when the stream dies** (`agent.rs:220-270`) —
   repeated interrupted execs accumulate orphaned root processes on the target.
9. **A second `serve` on one state dir silently steals the control socket**
   (`serve.rs:173`) — `remove_file` then `bind`, unconditionally, while the
   first instance keeps serving on an unlinked inode with the same NodeId.
10. **Unbounded `pending/`** and the reset-on-rejection backoff — together a
    self-inflicted amplifier.
11. **`classify_path`'s no-selected-path fallback** (`lib.rs:241`). Return
    `"unknown"` rather than guessing at the one place the spec forbids guessing.
12. **`forward`'s argument form** — take the spec's `<local-port>`, or record
    the deviation as the spec requires.
13. Minor: error masking on large failed pushes (`Broken pipe` instead of the
    agent's real message); `write_status_err` can emit a message longer than
    `read_status` will accept; `default_state_dir()` `Box::leak`s per subcommand
    field.

### What this does better than a spec-minimal implementation

A spec-minimal build would satisfy "expose the reachability state" with a log
line; this dials itself over a distinct ALPN and gives you a JSON file plus an
exit code. A spec-minimal build would report a path label once; this reports
every transition. A spec-minimal build would write `authorized_keys`; this pulls
it first and merges. A spec-minimal build would not have noticed that a wedged
session with zero open paths never produces a close frame. The comments are
almost uniformly *why*, not *what*, and I found none stale against current code
beyond the module-doc overclaim above. The instinct throughout is to close the
gap between "the code path exists" and "the operator can tell what happened" —
precisely the axis the spec cares about.

Which makes the defects more frustrating, not less: every one would have been
caught by running the artifact rather than inspecting it. The discipline that
produced the canary was never turned on the binary the canary exists to protect.

---

## Code-grade clusters

| Cluster | Grade | Note |
|---|---|---|
| correctness | **Work** | The integrity check verifies the transfer, not the content; broken two ways, neither with a test. Plus the destructive authorized_keys branch. |
| structure | **Good** | Sharp seams: routing that knows no protocol, ssh as near-pure composition. Docked for the hand-encoded format on both sides and `pull_to_memory`. |
| interface | **Good** | Command surface tracks the spec; `--json` where a program is the caller; `status`'s exit code is actionable. Docked for `forward`'s undocumented deviation and `u8`-const ops. |
| security | **Work** | Authority is the QUIC-authenticated public key and approval is fail-closed — the core is right. But the root-equivalent control socket rests on a directory mode that relocation silently drops, the temp path is symlink-attackable as root, and `pending/` is an unauthenticated unbounded write. |
| change | **Good** | The test suite is real and would catch regressions in most things — but in none of the defects found here. |
| writing | **Good** | Comments are why-not-what and overwhelmingly accurate; `PROOF.md` is a genuine artifact, not a brochure. Docked for the one false "proved" and the module-doc overclaim. |
| meta | **Good** | 2,641 lines for the whole deliverable, no feature bloat, docs lead with the point. |

**Bottom line for the merge:** take this branch's controller — the canary,
file-based approval, the routing bridge — and its ssh recipe's *structure*
essentially as written. Do not ship its `copy` until the integrity model is
fixed, do not ship its ssh recipe until the clobber branch is fixed, and do not
believe any static-musl claim until someone executes the binary.
