# Running FlowDMR alongside a live Brew (advanced)

The **default** FlowDMR build registers its entity under `TetraEntity::Brew`
(impersonation). That needs zero changes to FlowStation's protocol code, but it
means **you cannot run the real Brew interconnect at the same time** — which is
fine for a purely local cell.

If you want FlowDMR **and** a live Brew/BrandMeister interconnect at once, give
FlowDMR its own entity identity. This requires a small, local patch to
FlowStation's CMCE because the group-call path currently hardcodes the network
reply destination to `Brew` (the circuit/Asterisk path already echoes the
sender, so this just brings groups in line).

Build flag: add `flowdmr-entity/dedicated-entity` to the features, e.g.
`cargo build --release --features flowdmr` after also enabling
`dedicated-entity` in `bins/bluestation-bs/Cargo.toml`'s `flowdmr` feature:

```toml
flowdmr = ["dep:flowdmr-entity", "flowdmr-entity/dedicated-entity"]
```

## The six edits (all in your local FlowStation tree — do NOT commit/push)

### 1. New entity identity — `crates/tetra-core/src/tetra_entities.rs`
Append a variant at the **end** of `enum TetraEntity` (appending keeps the
`bitcode` discriminants of existing variants stable):
```rust
    /// Asterisk SIP/RTP bridge
    Asterisk,
    /// FlowDMR local DMR->TETRA injector
    FlowDmr,
}
```

### 2. Store the originating entity on the call — `cmce/.../cc_bs/...` ActiveCall
Add a field to the `ActiveCall` struct and to `ActiveCall::new_network(...)`:
```rust
pub network_entity: TetraEntity,   // who originated this network call (Brew / FlowDmr)
```
Default it to `TetraEntity::Brew` for any existing call sites you don't touch.

### 3. Thread the sender — `cmce/.../cc_bs/routes/ra.rs`
`rx_call_control` already binds `let src_entity = message.src;`. Pass it through:
```rust
CallControl::NetworkCallStart { brew_uuid, source_issi, dest_gssi, priority } => {
    self.rx_network_call_start(queue, src_entity, brew_uuid, source_issi, dest_gssi, priority);
}
```

### 4. Use it when allocating — `cmce/.../cc_bs/procedures/isi.rs`
In `rx_network_call_start`, accept `src_entity: TetraEntity`, store it on the
`ActiveCall` you insert, and send `NetworkCallReady` to `src_entity` instead of
the hardcoded `TetraEntity::Brew` (the send near isi.rs:881):
```rust
dest: src_entity,   // was: TetraEntity::Brew
```

### 5. Reuse path — `cmce/.../cc_bs/procedures/group.rs`
In `fsm_group_on_network_call_start`, the `NetworkCallReady` send (group.rs:313)
should target the call's stored entity:
```rust
dest: call.network_entity,   // was: TetraEntity::Brew
```

### 6. Teardown — `cmce/.../cc_bs/lifecycle.rs`
The `NetworkCallEnd` push (lifecycle.rs:142) should look up the call and use its
`network_entity` instead of `TetraEntity::Brew`. If the call record is already
gone, falling back to `Brew` is harmless.

## Why this is safe

- Appending the enum variant doesn't shift existing `bitcode` discriminants, and
  FlowStation routes messages via a `HashMap<TetraEntity, _>` (no exhaustive
  matches in the runtime path), so nothing else breaks.
- The change only generalises the group-call reply target the same way the
  circuit-call path already works — Brew keeps receiving its own replies because
  its `NetworkCallStart` carries `src = Brew`.

After these edits, FlowDMR uses `TetraEntity::FlowDmr`, the real Brew keeps
`TetraEntity::Brew`, and both can run concurrently. FlowDMR's local-only GSSI is
still rejected for Brew routing by `local_ssi_ranges`, so its traffic never
leaves the cell.
