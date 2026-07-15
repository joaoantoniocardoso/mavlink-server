# mavlink-server: follow-ups after mavlink-codec `wip2` PacketRef work

## 0. Bump `mavlink-codec` first

`Protocol::sequence()` needs `MAVLinkMessage::sequence()` from a recent `wip2`
revision. Until that is on the remote, point Cargo.toml at the local tree:

```toml
mavlink-codec = { path = "../mavlink-codec", features = ["json"] }
```

Then restore the git dep (with a pinned `rev`) once pushed.

## 1. Prefer in-place typed parse

`Protocol::to_mavlink` still uses async rust-mavlink peek/read. Replace the body
with the zero-copy path (no frame copy into `MAVLinkV*MessageRaw`):

```rust
let Some(packet) = self.wire() else { /* same error as today */ };
packet
    .as_ref()
    .to_mav_message::<M>()
    .map_err(anyhow::Error::msg)
```

`to_mavlink_json` can keep calling `to_mavlink` once that is fixed.

## 2. JSON transcoder call sites

`rt::to_json` / `rt::to_json_indexed` accept `PacketRef` **or** `&Packet`
(via `From<&Packet> for PacketRef`). Existing:

```rust
rt::to_json_indexed(packet, desc, &mut blob, &mut ranges);
```

where `packet: &Packet` keeps compiling. `packet.as_ref()` also works.

## 3. New header helpers

- `MAVLinkMessage::sequence()` / `Protocol::sequence()` — same cheap wire-or-JSON
  resolve as `system_id` / `component_id`. Use these in filters/stats instead of
  forcing a full typed parse when you only need the sequence byte.

## 4. Encode path / `Framed::split` inference

`MavlinkCodec` implements `Encoder<PacketRef<'_>>` and `Encoder<Packet>` (the
latter delegates through `as_ref()`). Prefer `encode(packet.as_ref(), …)` only
when you already hold a borrowed frame; keep sending owned `Packet` from
`Framed` sinks.

When the sink item type is not constrained (e.g. the writer half is unused),
annotate `split`:

```rust
// TCP / serial Framed
framed.split::<Packet>()

// UDP
UdpFramed::new(socket, codec).split::<(Packet, SocketAddr)>()
```

## 5. What not to chase

- No streaming `Decoder<Item = PacketRef>` — lifetimes don’t work with
  `BytesMut` / `Framed`. Decode still `freeze()`s into `Bytes`; use
  `packet.as_ref()` afterward.
