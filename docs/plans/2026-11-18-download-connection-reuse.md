# Reusing download connections within the cap of 6 (#16, PR #121)

## Problem

PR #121 bounds the open sockets to a datanode by closing every download
connection with its response (`Connection: close`, no idle pool). Each
file therefore costs a TCP and TLS handshake to archive.org and another
to the datanode. Measured on 2026-11-18
(`docs/plans/2026-11-18-parallel-download-qa.md`), an item of 1,267
files of a few kilobytes took 26.0 s against 14.3 s for v0.27.0 and
opened 2,541 connections against 17; 497 files of about 190 KB took
22.6 against 11.1 s. That fails the bar for release (faster and more
efficient than v0.27.0) and costs the servers 2,500 handshakes.

Goal: connections are reused across files and parts, and the bound still
holds: at most 6 established sockets, busy or idle, to a datanode pair
from one machine, across every `ia` process. The contract text does not
change.

## Why a shared pool does not work

reqwest (hyper-util underneath) cannot keep a pool under a hard socket
count:

- **The pool races a new connection against the return of an idle one.**
  When a request finds no idle connection, hyper starts connecting and
  watches the pool at the same time. If the idle connection comes back
  first (it is returned by a spawned task, asynchronously, after the
  previous body ends), the request uses it and the new connection is
  finished in the background and pooled. That is the seventh socket seen
  on 2026-11-05.
- **The pool is per host; the cap is per pair.** A pool sized to 6 per
  host can hold 12 to a pair.
- **No hook on socket close.** reqwest's connector layer must return its
  sealed `Conn` type, so a socket cannot be wrapped to learn when it
  closes, and `tower` is not a dependency.

What reqwest does expose is a DNS resolver per client
(`ClientBuilder::dns_resolver`), called once for every new connection to
a host name.

## Design: a connection owned by its slot

A slot becomes a long-lived object, not only a permit.

- **One client per slot, one connection per client.** Each slot owns a
  `reqwest::Client` built for it (HTTP/1.1, keep-alive, at most one idle
  connection per host, the download transport's headers and timeouts)
  whose resolver is a gate: the first resolution goes through, every
  later one fails at once, before any socket exists. A slot's client can
  therefore open one connection in its life. Requests on the slot reuse
  it while it lives.
- **A refused connect is retried, then the client is replaced.** A
  refusal means hyper found no idle connection on the slot: either the
  previous connection has not been returned to the pool yet (the race
  above), or it is gone (closed by the server, broken by an error, or
  abandoned mid-body by a shed or stall). No request was sent. The hop
  sends again after 5 ms, up to 10 times, which covers the return; after
  that the slot retires its client (dropping it, which closes any socket
  it still holds) and builds a new one after a grace of 100 ms, so the
  old socket is closed before the new one opens. A response with
  `Connection: close` or a failed send marks the client spent, and the
  next request on the slot replaces it at once.
- **Idle slots.** When a request ends, its slot keeps its client and its
  idle connection, and its slot file stays locked, if the server's slots
  with a connection (in use and idle) are fewer than the live budget;
  otherwise the slot retires. The next request to that server in the
  process takes an idle slot first, one already connected to the same
  host by preference. A slot idle for 2 s retires: its client is dropped,
  and its slot file is unlocked 100 ms later.
- **A pair's two hosts.** A slot is connected to one host. A request to
  the other host of the pair takes an idle slot of that host, else a slot
  with no connection, else retires an idle slot of the first host (grace
  as above) and reuses its slot file.
- **The archive.org front hop** is a server like any other (its own key,
  its own 6 slots), so it gets the same reuse.
- **Host given as an IP address.** hyper skips the resolver for an IP
  literal, so the gate cannot hold there. A request whose host is an IP
  address keeps the current behaviour: the shared transport with no
  idle pool and `Connection: close`. archive.org's hosts are names; only
  a `-H` override or a test server is an address.

## How the bound of 6 is kept

- **Within a process:** a server has at most `max` (6) slots. A slot has
  at most one socket open: the gate refuses a second connection of its
  client, and a retired client's socket is closed before the grace ends
  and the slot's next client may connect. Slots in use are bounded by the
  live budget (the existing semaphore); idle slots are kept only while
  in-use plus idle stay within the budget.
- **Across processes:** every slot that has, or may have, a socket holds
  its slot file locked: from the moment it is taken until 100 ms after
  its client is dropped. An idle connection therefore holds its file, and
  another process cannot take that slot while the socket lives. The free
  slots counted for splitting (`room`) count this process's idle slots as
  free, since its own requests take them first.
- **What a datanode can see:** at most one socket per locked slot file,
  so at most 6 per pair from the machine, busy or idle.

## A 429 or 5xx drop

The budget halves as now (6, 3, 2, 1). Every idle slot of that server
retires at once: idle sockets close first. Range streams, then whole
files, are shed as now if the slots in use still exceed the budget, and
their slots retire on release because in-use plus idle exceeds the
budget. The budget grows back by one per calm minute as now.

## What is given up

- **Other processes wait up to 2 s longer.** A process holds its idle
  slots (and their files) for up to 2 s after its last request to that
  server, so another process may wait that long for them. Within a
  process an idle slot is taken at once.
- **Guarantee level.** The close of a retired client's socket is done by
  hyper's connection task, asynchronously; the 100 ms grace before the
  slot's next connection and before its file is unlocked covers it, but
  is not a proof. This is the same kind of guarantee as the current code,
  where the socket closes in that task after the response ends.
- **A slot's TLS sessions** are not shared with other slots' clients, so
  each slot's first connection does a full handshake.
- **IP-literal hosts** do not get reuse (above).
- **HTTP/2** stays off: one connection per transfer, as measured on
  2026-10-30.

## Tests (hermetic, loopback, counting TCP connections at the server)

Servers are reached as `localhost:<port>`, so the resolver gate is used.

1. Forty small files on one server, `-j 8`: at most 6 TCP connections
   in all, and at most 6 open at once; most requests share a connection.
2. An item's pair with a front server that alternates the redirect: at
   most 6 sockets open on the two nodes together, idle ones included, at
   every moment, and none once the slots have retired.
3. Two `ia download` processes on one cache directory: at most 6 open
   sockets on the server together.
4. A 429 with idle slots: the idle connections close at once, before any
   in-use transfer is shed.
5. A large file still splits into 6 ranges; its parts reuse the streams'
   connections.

The existing tests that assert one request per connection and
`Connection: close` change to the IP-literal case, which keeps that
behaviour.
