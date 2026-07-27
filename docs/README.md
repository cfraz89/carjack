# carjack docs

Four documents, split by how fast they change:

| | | |
| --- | --- | --- |
| [status.md](status.md) | **Start here.** Where things stand, what is untested, what to do next. | Changes every session |
| [investigation-log.md](investigation-log.md) | Dated, newest-first record of what each capture showed and what it changed - including the wrong turns. | Append-only |
| [captures.md](captures.md) | What each `.pcapng`/`.pklg` in the repo root contains, and what the iPhone reference recordings established. | Occasionally |
| [tooling.md](tooling.md) | Why the build/run harness is shaped the way it is. | Rarely |

**Durable facts about how the code works live in `CLAUDE.md`, not here** - the architecture, the
encode backends, the protocol rules that are now enforced in code. If something is true of the
code as it stands and will stay true, it belongs there. These docs are for the state of the
*investigation*: what has been tried, what is believed, what is next.

Practically: when a session ends, update `status.md`, append to `investigation-log.md` if a
capture was analysed, and promote anything that turned out to be a durable rule into
`CLAUDE.md`.
