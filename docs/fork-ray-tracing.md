# Ray tracing in this fork: what the title does and what has been built

The title's traversal programs already contribute to the picture. Holding
them in the fork's `VICTORY_BVH_HOLD` experiment removed indirect light
(fork commit `a253706`). That switch is absent on this branch. The dynamic
builder is off by default. Its output has yet to be verified end to end.

## What the title does

The title's indirect light (probes, bounce, reflections) and the
shadows of its lamps and sun are ray traced. Without them, anything the sun
does not hit directly is black: interiors, the shadowed side of a face, rooms
lit only by lamps.

The title does not use a ray-tracing API:

- Traversal is ordinary compute-shader code: a per-thread stack in shared
  memory, and loads through raw 40-bit addresses of title memory.
- The static world's acceleration structures ship pre-built in the title's
  data, in the console GPU's native node format (64-byte nodes: box nodes with
  four children and 16-bit float bounds, 128-byte box nodes with 32-bit float
  bounds, triangle nodes, instance nodes). They are unpacked on the GPU.
- A set of builder programs (from the title's `XboxBvhBuilder.dll`, 52 compute
  programs) builds and refits the dynamic structures on the GPU every frame.
- Only the per-node test is hardware: the instructions
  `image_bvh64_intersect_ray` and `image_bvh_intersect_ray`. One call tests a
  ray against a box node's four children (returning the hit children, nearest
  first) or against a triangle node (returning the intersection's numerator,
  denominator and barycentric numerators).

The fork translates the builder and traversal as compute shaders and
emulates the per-node instruction. This preserves the title's ability to
inspect each child, apply an instance transform and read its own leaf data.
Native DXR would require recovering geometry, instance masks, transforms
and hit identifiers, then replacing the traversal loops in the consumers.
A DXR acceleration structure also has an opaque layout. Its address cannot
serve a shader that loads the title's own nodes by byte offset.

## What this fork adds

1. **The builder programs translate** (49 of 52; all that the title creates in
   the scenes tested). They pass buffer addresses in a spilled parameter table
   rather than in descriptors. Added: the spilled user-data table, root
   unordered-access views (translator and host binding), byte and 16-bit buffer
   accesses, and several control-flow shapes.
2. **Title memory by raw address from shaders.** A directory with one entry per
   64 KiB of title address space leads a shader to the layer's host copy of
   that memory; loads, stores, atomic add/sub and float min/max go through it.
   Before a dispatch the host makes ready the memory its root views and root
   constants name.
3. **The ray test, emulated in DXIL.** Four slab tests and a sorting network
   for box nodes; a watertight ray/triangle test for triangle nodes; both forms
   of the instruction. A plain Rust reference lives in
   `crates/victory-shader/src/bvh.rs`; tests run the emitted DXIL on the GPU
   over 311 ray/node cases and compare with the reference bit for bit.
4. **The trace and shading programs translate** (59 of 64 trace programs, and
   the irradiance-cache shading program). This needed lane masks treated as
   plain numbers on some paths, and exact forms for programs written for
   64-lane waves running on a GPU with 32-lane waves.
5. **A safety rule on the host side.** The builder's stages run only while
   every builder pipeline the title has created has a host pipeline; otherwise
   none run (`VICTORY_BVH_BUILDER=1` asks for it; `VICTORY_BVH_QUARANTINE=1`
   also keeps the builder's stores out of title memory, for a first test). A
   partly working builder wrote garbage bounds into title memory and crashed
   the title.

## Known gaps

- With `VICTORY_BVH_BUILDER=1` the builder still stays off in a real run: a few
  of its pipelines are refused by the host (they still ask for 64-lane waves on
  a 32-lane GPU) or do not translate.
- Five shadow-mask trace programs do not translate yet.
- An access to a title page that has no host copy is lost (a read gives zero, a
  store is dropped).
- Not publicly documented, and assumed: the instruction's rounding, the order
  of two boxes entered at the same distance, and the result for a ray exactly
  through an edge.
- Cost on the GPU is unmeasured.

## Bringing up the guest-format builder

The design notes attribute about 515 dropped operations per heavy frame
to the builder. These are compute dispatches counted with dropped draws.
The 300-second log has 45 intervals with this drop reason, from 259.7 to
1,907.0 per frame, with a median interval of 343.1. It records use of
raw-address shaders and no missing-page report. Holding the walkers
changes the picture even with the builder off, so keep static traversal
active throughout the dynamic-builder experiments.

1. **Read resident children.** Implemented for the raw-address compute
   traversal under `VICTORY_BVH_RESIDENT_BOXES=1`. Directory entry zero
   means no host copy. A missing page reads as zeros and a ray through
   the origin can accept those zeros as both a box and a child pointer.
   Check the copy numbers from the node's existing lookups before sorting
   the hits. All four child pointers are in the first 64 bytes. A box
   with half-float bounds fits there, as do the first two bounds of a
   full-float box. Children 2 and 3 in the latter also need the next 64
   bytes, which can occupy another page and another host buffer. The fault
   report still requests each missing page. A genuinely resident zero
   pointer and a zero-sized box retain their ordinary intersection rules.
   Setting the switch to 0 or leaving it unset uses the previous box test.
   The synthetic GPU regression binds both halves through the real page
   directory, crosses a 4 GiB boundary, and checks all four combinations
   of present and absent pages with both pointer widths. It checks which
   faults reach the host and follows the first returned child to compare
   its triangle distance and barycentrics with the known geometry.
2. **Prove a complete bottom-level build in isolation.** Supply synthetic
   triangle buffers, scratch, output and the builder's spilled arguments
   through the regular list and page bindings. Require every stage's
   host pipeline, the correct wave semantics, mapped writable extents
   and ordered writes between dependent passes. Check inter-group
   publication of child bounds in bottom-up construction. Start with
   one triangle, then multiple workgroups. Read the header's root pointer
   at byte 48 and walk the result. Compare hit distance, primitive ID
   and barycentrics against independent brute-force triangle intersections.
   Repeat after moving vertices through the refit path. Check scratch
   and output guard bytes after the fence.
3. **Prove instancing and publication.** Build a top-level structure over
   two bottom-level structures. Exercise transformed instances, masks,
   high addresses and distinct geometry and instance IDs. Compare
   world-space hits before and after a transform update. Keep static
   unpacked structures in the same address space. Track scratch, inputs
   and output through completion, including aliases and address reuse.
   A missing write page loses work for that build. Stage complete results
   privately and publish only after checking dependencies and completion.
   Retain the last complete result or the ordinary disabled behaviour on
   failure. The existing quarantine suppresses CPU copyback only and
   cannot provide that publication boundary.

**In-game comparison.** Leave the builder off for the first A/B. Use
matching views of indirectly lit surfaces and compare shadow edges and
frame-to-frame flicker. Follow the per-frame builder-off drop reason,
dispatched compute work and the `title memory by address` missing-page
reports. Those reports count distinct reported page slots across all
raw-address shaders, can collide, and stop printing after 50 reports.
A stable builder-drop count is expected from this traversal change.
Check the submit time and card-busy line separately. The number of
correct hits and the picture change require an in-game comparison.

### Reference findings

- XCL `a21d3df`, `crates/xcl/src/gpu/bvh.rs`: a guest-format builder
  and CPU tracer provide a way to check headers, nodes and hit records
  against geometry. Its instance layout includes fields inferred from
  shader use. Refitting and parent links require separate verification.
- XCL `b56a1b2`, `crates/xcl/src/hle/d3d12x/console_list.rs` and
  `crates/xcl/tests/console_dlls_e2e.rs`: replay the builder's compute
  commands and validate a bottom-level build against brute-force rays.
- XCL `7495271`, `crates/xcl/src/hle/d3d12x/console_list.rs` and
  `bind.rs` in that directory: bound the inline arguments by the shader's
  user-register count. The wrong split produced empty structures even
  though dispatches completed. This fork already rewrites spilled loads.
- XCL `0fb82c6`, `crates/xcl/src/gpu/desc.rs`, `gpu/bvh.rs`,
  `gpu/recomp/emit.rs` and `hle/d3d12x/list_x.rs` under the same source
  root: preserve acceleration-structure descriptors, use the complete
  node address to select memory, and order dependent builder dispatches.
  This fork uses page-directory copy numbers and a fault buffer.
- XCL `f7bd213`, `crates/xcl/src/gpu/bvh.rs` and
  `crates/xcl/src/gpu/dxil/lower_image.rs`: select the proper vertices
  for triangle types 2 and 3. The fork's CPU and DXIL tests already
  cover all four types.

## Where to look

- `crates/victory-shader/src/bvh.rs`: the reference for the node test.
- `crates/victory-shader/src/emit/lower.rs`: the lowering of the two
  instructions and of memory by address (`Lower::memory`).
- `crates/victory-shader/src/resource.rs`: address tracking for descriptors
  and raw addresses.
- `crates/victory-gpu/src/pipeline.rs`, `draw.rs`: the builder rule and the
  host side of root views and title memory.
