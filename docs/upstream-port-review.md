# Upstream port review

The comparison starts at local commit 529b997. The fork range ends at d510ee4 and the upstream range ends at 13f91ca. The tables cover six fork commits, 265 upstream non-merge commits and 137 upstream merges. Pull-request titles omit contributor and branch handles. Each SHA identifies the original subject and author.

The 60-second CPU sample found 160 ms/s in NtProtectVirtualMemory, 131 ms/s in shadow lookup and 128 ms/s in NtGetWriteWatch across all sampled threads. The heavy frames spent approximately 86 ms working on the submit thread and 15 ms waiting for the relay. Compare present intervals within the same guest-draw-count band.

The 408 verdicts comprise 17 changes already in this branch, 15 superseded by local work, 174 useful changes and 202 that do not apply. Useful entries identify a local port or work for the indicated area. Historical measurements refer to the tree that recorded them.

## Local ports and A/B switches

All switches below default to **off**. Set one to `1` in a fresh process to exercise that port. Unset or `0` selects the tested baseline. The fork measured CPU placement on another processor. Guest visuals, device-loss handling and scheduling still need a local A/B. The synthetic tests check arithmetic, identities, ownership and ordering.

| Environment variable | Source | Change and measurement risk |
| --- | --- | --- |
| `VICTORY_FAST_CORES` | 0b0074e | Ask Windows for fast CPU sets on the submit, relay and late-wait workers. Skips existing CPU-set policy, multiple processor groups and uniform eligible cores. |
| `VICTORY_DESCRIPTOR_CURSOR_RUN` | c1c3281, 072fde0 | Finish a contiguous dynamic descriptor allocation past the search cursor. A fragmented heap may need up to one request width of additional scanning. |
| `VICTORY_SKIP_ALIAS_COPY` | a253706 | Skip a whole-resource self-copy or an identical same-subresource region. Retain upload, twin and write tracking. Displaced regions still go through the original copy path. |
| `VICTORY_PRIVATE_DATA_SEEK` | 3e98bd7 | Remove only the dying COM object's private-data range under the global lock. Benefit depends on the number of unrelated live entries. |
| `VICTORY_SHARE_SWIZZLE_TABLES` | 64b80c3 | Share immutable coordinate offset tables between matching texture layouts. At most 45 table keys live until process exit. |
| `VICTORY_DXBC_FIXED_HASH` | 711d651 | Use fixed checksum constants and a stack block when writing DXBC. This addresses translation and cold loading. |
| `VICTORY_ALIGN_APPENDED_INPUTS` | 691236d | Align appended vertex elements per input slot in the CPU rectangle fallback. |
| `VICTORY_RECT_NORMALIZED` | 6f36cdf | Interpolate additional normalized vertex formats in the fourth rectangle vertex. |
| `VICTORY_RECT_STRIDE_GUARD` | e3c9dd0 | Leave the destination untouched when a rectangle stride is zero or six strides exceed the address space. |
| `VICTORY_A8_FORMAT_FAMILY` | 97c8783 | Keep alpha-only A8 out of the red-only R8 typeless family. |
| `VICTORY_RESERVE_FRAME_TICK` | 810f442 | Reserve successful waits before releasing the clock lock. Concurrent waiters for one event can now complete on successive ticks. |
| `VICTORY_DEVICE_REMOVED_REASON` | 13185f5 | Report the host removal status through the guest device. A caller may now enter its device-loss recovery path. |
| `VICTORY_FENCE1` | bbdc3ca | Expose the existing fence creation-flags method through the Fence1 interface. |
| `VICTORY_VOLUME_SLICE_BOUNDS` | 93a24b7 | Reject tiling or detiling into overlapping volume slices before copying bytes. |
| `VICTORY_LINEAR_MSAA_CHECK` | 350f91c | Reject linear multisample texture descriptions whose storage only covers one sample. |
| `VICTORY_XMA_FRAME_BOUNDS` | d4d1822 | Bound a declared frame length by its real payload. |
| `VICTORY_PE_SECTION_BOUNDS` | e57e76a | Reject out-of-file raw section extents before loading. |
| `VICTORY_EXPORT_ALIASES` | d0dd9fc | Keep every exported name sharing an address-table ordinal. Check modules that use forwarded names and ordinal-only exports. |
| `VICTORY_IMAGE_STAGING` | 1aac20a | Give each prepared image writer an exclusive temporary file. A failing publisher cleans up its own partial output. |
| `VICTORY_UNLOCK_DLL_LOAD` | c6959ac | Let lookups proceed during a native DLL load and balance duplicate native references. Loading callbacks can reenter the module cache. |

The query and command-signature tests from 0be58e6 change no behavior. The worktree has the original test patch and hand-adapted runtime ports. Git could not create the shared worktree index lock for the requested history-preserving cherry-pick. The source SHA in each row identifies the original change.

### Reference and measurements

XCL 7b029e2, `crates/xcl/src/hle/kernelx.rs`, fits guest processor requests to the process affinity allowed by Windows. The port uses the fork's CPU-set idea for engine-owned workers and independently filters eligible processors. The affinity restriction is credited in `fast_cores.rs`.

For the CPU-set A/B, check `fast core selection` with the worker name, `fast_sets` and `applied`. Compare `KELVAROQ_PRESENT interval_us` within matched guest-draw bands using deltas of `KELVAROQ_QUEUE_TOTAL draws`. Compare the submit thread's "work and other time" and "awaiting relay" lines alongside the relay's work time. The fork observed the submit worker on slower cores in 6% of samples. The frame-time effect on this machine still needs measurement.

For the descriptor change, compare "dynamic slots" tables asked, refused and search time, and guest draws against drawn commands. The supplied run has no exhaustion warning. Its last recording summary asked for 11,546 tables per frame, with zero refusals and 0.8 ms searching. The earlier local run that exhausted the heap is the relevant correctness case. The alias guard logs `identical texture copies skipped` at powers of two with a cumulative `skipped` count. Compare matched images for the rectangle, format and alias changes. Loading-only switches need cold-load time and cache results from matching inputs.

### Parallel integrations

| Area | Changes to compare with local work |
| --- | --- |
| Submission | 3973179 joins adjacent queue parts. 3e10052 moves release of completed GPU objects off the submit thread. Preserve queue waits and bounded resource lifetime. |
| Relay and coherence | 3973179 copies back through an owned view. ca8829f compares changed bytes on refresh. cd94d35 introduces shader page-write marks. Each changes ownership across in-flight writes. |
| Indirect | 465b94f gathers scattered records. Compare on non-mesh records left after 0041 and keep barriers between producer writes and indirect reads. |
| Descriptor heaps | f809521 snapshots a changed page under one lock. cea278c and 42ea09f move expensive view work outside slot locks. Any publication must revalidate a rewritten slot. |
| Textures and VRAM | cc5f961 maps changed pages to subresources. 8a3d975 suballocates upload space. ea81289 batches transitions. Check shared mip tails and remaps. |
| Pipelines and shader compilation | f821cb9 loads distinct names concurrently. 9c58a1c prebuilds packed PSOs. 54ab406 is the final nested-descriptor join after an earlier revert. |
| Wave64, ray tracing and service correctness | The individual rows identify the instruction, stage, API or lifetime affected. 0b35b7f and 27c3dc9 depend on host and translator changes together. |

The two mapping experiments documented in d510ee4 stay out. The fork found no redundant remaps to skip, and grouping remaps produced visible corruption. Its earlier draw-window comparison used different scenes and was withdrawn.

## Verification

Each suite ran with Rust 1.98.1 in two fresh environments. All 20 switches were set to `0` in the first and `1` in the second. The GPU library suite used one test thread. New tests use synthetic inputs.

| Suite | Passed with switches off | Passed with switches on | Ignored per run |
| --- | ---: | ---: | ---: |
| GPU library | 219 | 219 | 12 |
| Shader | 218 | 218 | 3 |
| HLE unit and integration | 104 | 104 | 0 |
| Formats library | 29 | 29 | 4 |
| Loader library | 26 | 26 | 0 |
| PE library | 11 | 11 | 0 |
| **Total** | **607** | **607** | **19** |

The locked `diag` build of the `victory` binary with `diagnostics` succeeded for `x86_64-pc-windows-msvc`. The measurements above come from the supplied recordings and upstream history. Heavy-frame time and image quality with these ports still need an in-game A/B.

## Fork

| SHA | Title | Verdict | Reason |
| --- | --- | --- | --- |
| a253706 | Frame rate, third session: a frame log and a card meter, the driver watch, a forbidden self-copy removed; draw windows measured at 6.0 -> 8.9 frames a second | Useful | Adapted the host-texture self-copy guard. Patch 0044 already supplies direct reads, 0046/0048 supply pacing and 0006/0042 supply frame counters. Driver-watch/window coupling needs the indirect branch. |
| 0b0074e | Copies in place measured and fixed, our threads on the fast cores, and a corrected comparison of draw windows | Useful | Adapted fast CPU sets for the submit and completion workers. The 256-byte threshold and fault cooldown are in 0044. Disk-worker placement has no local CPU contention measurement. |
| 3973179 | What a frame is made of, measured; parts joined and copy-back through our own view (9.6 fps in the first rooms) | Useful | Submission group should evaluate JOIN_PARTS with 0021/0036 ordering and lifetime tests. Relay group should evaluate BACK_OWN_VIEW with alias reuse and remapping. The fork measured about 6% and 9% respectively on its workload. Its SPREAD/REFRESH_ONCE/VIEW_KNOWN/WINDOW_JUMP trials did not pay. The separate 15-byte zeroing shortcut is a title-thread optimization. |
| 465b94f | Scattered indirect records clamped once per list (VICTORY_INDIRECT_GATHER=1): 10.3 fps in the first rooms | Useful | Indirect group. Compare GATHER to the existing amplification-stage path (0041) and batching branch, particularly for scattered non-mesh records. Per-list snapshotting must respect guest barriers. |
| ca8829f | The frame rate in the corner of the picture; changed-bytes refresh of card copies (off) | Useful | Relay group. Changed-byte refresh needs the card-copy baseline, exact dirty extents and in-flight write tests together. The overlay duplicates external present timing and changes the picture under test. |
| d510ee4 | README: the dips while moving, measured; two attempts on the title's page mapping, both taken back out | Not applicable | Measurement record. Keep both withdrawn remap experiments out. Every sampled page really changed and grouping real remaps produced corrupt textures. |

## VIctory upstream

| SHA | Title | Verdict | Reason |
| --- | --- | --- | --- |
| f5cb5ff | Capture startup breakpoint diagnostics | Not applicable | Startup breakpoint tracing covers a failure before the measured workload begins. |
| a3acc87 | Check the required startup archive before executing Series 1.0.2.1 | Not applicable | Requires an archive specific to one build. The generic loader serves multiple packages. |
| 628b95d | GPU: hold SHADER_HEAPS across descriptor mirror and check query offsets | Useful | Descriptor/correctness group. Couple mirror lifetime under SHADER_HEAPS to query-write footprint checks in list and resource code. |
| ba42db6 | xma: start the frame after a continuation at the next packet that has frames | Useful | Audio service group. Continuation packets affect frame boundaries and need the decoder and ring tests together. |
| d01b170 | XAsync: end the call that finished, not whatever call the block holds now | Useful | Service correctness group. Completion must own the original call when an XAsyncBlock is reused. |
| b1bdd89 | Census: report directory traversal failures | Not applicable | The offline census tool does not run during loading or rendering. |
| e5b7159 | Shaders: walk a lane-mask diagram once in vars and given | Useful | Recompiler group. Memoization touches the divergent-mask decision-diagram implementation used by wave lowering. |
| b9ff219 | XTaskQueue: terminating a queue cancels its delayed callbacks too | Useful | Service correctness group. A terminated queue can retain delayed callbacks. Check ordering against the XAsync completion changes. |
| 01ae55a | Files: a title run by its file name alone gets its own folder as G: | Not applicable | The recorded launches mount an explicit package directory. This changes bare-name CLI launches. |
| 4eb0898 | XMA: build the inverse transform's rotations once per subframe size | Not applicable | Reduces inverse-transform work on audio decoding threads. The profile identifies submission as the frame limiter. |
| 810f442 | GPU: a frame wait takes its tick before it sleeps | Useful | Adapted frame tick reservation under the clock lock. The existing late-wait rule still consumes an elapsed tick. A timeout consumes no tick. |
| d4d1822 | XMA: noise tests, and end a frame at its data instead of looping forever | Useful | Adapted the XMA reader end bound. Synthetic bit-length tests cover a short final byte and a length beyond the supplied payload. |
| 3c7d845 | Kernel: a title image's file name that fits clears the last error | Useful | Service correctness group. On successful short GetModuleFileName calls the shim must clear last error. |
| 985d8d4 | Audio: an XMA loop's skip is of its first frame, not of its last | Useful | Audio service group. Loop skip belongs to the first frame. Pair with continuation and ring-wrap behavior. |
| 64484b4 | Input: apply VICTORY_KEYS to a connected host pad too | Not applicable | Input mapping for a connected pad has no bearing on the measured stalls or missing output. |
| c63262f | Merge upstream main into startup breakpoint diagnostics | Not applicable | Resolves overlapping edits in crash.rs. The merged handler first emulates the instruction and then applies the startup breakpoint diagnostics. |
| 7f00a62 | Crash reports: name the host's CPU and Windows build | Not applicable | Additional host identifiers in crash reports. The requested runs already identify the hardware and OS. |
| 11ca3ce | Media: a share is refused however it is spelled | Useful | Service correctness group. UNC parsing shares the media URI validation path with 75fe6c0. |
| 0de9efc | Loader: clzero loops in one exception; rdpru | Already in ours | The pin already handles CLZERO loops and RDPRU in console_insn and console_insn_decode. |
| e57e76a | fix(pe): validate section raw data bounds | Useful | Adapted section raw-extent validation at PE parse time, including zero-length and exact-EOF cases. |
| acdcc41 | Loader: suspend a thread before reading its TEB; skip bad dump times | Not applicable | Applies to the optional scheduled stack dumper. The frame workload runs with sampling separate. |
| 5cff000 | XAsync: a new block where a result was left behind begins a call | Useful | Service correctness group. A new block can reuse the address holding an abandoned result. Pair with d01b170. |
| 1054fe8 | Audio: an XMA read block past the ring's end wraps instead of filling forever | Useful | Audio service group. Read-position wrap controls decoder progress and needs paired write-position tests from 7a5d85e. |
| 92f2c60 | GPU: every object answers for IUnknown | Useful | D3D12 correctness group. QueryInterface identity is shared across all object vtables, including resources and fences. |
| b58d198 | GPU: copy into title memory without asking for its region first | Already in ours | copy_mapped already asks about the destination only after a failed bulk read. VICTORY_COPY_ASK selects the earlier query. |
| e3bef08 | XStore: a store id without an installed package is an invalid argument | Not applicable | Offline store error handling concerns package lookup, outside the rendering and loading paths under study. |
| c6959ac | HLE: load a host DLL outside the module cache's lock | Useful | Moved native DLL loading outside the module cache lock on request. Concurrent publishers retain one reference and release the extra load. |
| 93a24b7 | fix(formats): reject overlapping texture depth slices | Useful | Adapted depth pitch validation in the shared texture walker. Both tiling directions check for overlap before writing. |
| d549b63 | HLE: a '>' in a quoted manifest attribute does not end its tag | Useful | Service correctness group. Manifest parsing must preserve quoted delimiters together with comments in ca362a4. |
| e6ebec9 | Shader info: show stage I/O even when instruction decoding fails | Not applicable | Changes the offline shader inspection command's output after a decoder error. |
| 3f5eff5 | input: with no device, previous and next readings walk every device in time order | Not applicable | Input-history iteration for all devices has no measured counterpart in this run. |
| 936e5e5 | XGameUi: a text entry's limit is the console's | Not applicable | Changes the text-entry dialog's character limit. |
| c000581 | Kernel: GetModuleFileName of a title image into a null buffer | Useful | Service correctness group. Null output handling belongs with the related GetModuleFileName last-error fix. |
| 7d04943 | Shaders: a signed 64-bit operand sign-extends its 32-bit literal | Useful | Recompiler group. Signed scalar literal extension affects 64-bit lowering, with instruction-level regressions upstream. |
| e6fe8ec | GPU: fix CreateSamplerX mip-none base level, LOD range, bias clamp, and invalid enum values | Useful | D3D12 correctness group. Port sampler enum, bias and LOD handling together with e34cb53 and feature-dependent filtering. |
| 2c22e2b | XStore: wire offline *Result slots and fix OOB XStoreShowProductPageUIResult slot | Useful | Service correctness group. Offline store result slots are ABI-sensitive. Port the result functions and vtable tests together. |
| a3f89ed | Census: deduplicate PE imports without hint | Not applicable | Changes import deduplication in offline census reports. |
| 75fe6c0 | HLE: require two hex digits in percent escapes | Useful | Service correctness group. Tightens media decoding of percent-escaped paths. Review alongside 11ca3ce. |
| b956320 | GPU: handle depth/stencil copy footprints | Useful | Texture group. Plane-specific depth/stencil footprints feed texture copies, subresource indices and staging sizes. |
| 65062e1 | xmem: report allocations the host refuses | Not applicable | Adds logging for refused guest allocations. No associated refusal determines the sampled steady-state frame time. |
| 7a5d85e | Audio: an XMA write block past the ring's end wraps before the first write | Useful | Audio service group. A write cursor past the ring must wrap before a write. Pair with 1054fe8. |
| b796c8b | GPU: clear DXGI outputs on out-of-range enumeration | Useful | D3D12 correctness group. Out-of-range adapter/output enumeration must clear the returned COM pointer on the failure path. |
| 705a2f6 | Inline uniform micro-tile runs in texture copies | Already in ours | The pinned formats layer splits the final short run and has fixed-width copy_runs specializations for tiling and detiling. |
| 81b81df | Shaders: reuse lane guards for unchanged EXEC within a block | Useful | Recompiler group. Guard reuse depends on identifying every write to EXEC within each block. |
| e950e81 | xtask: count \x escapes and 0x hex arrays as encoded runs in the policy check | Not applicable | Offline repository policy scan. No runtime path. |
| a4974e4 | GPU: VICTORY_* switches set to 0 are off | Not applicable | Global reinterpretation of older switches would change existing explicitly configured behavior. Each new port here has its own opt-in. |
| 691236d | GPU: align appended input-element offsets like D3D12 | Useful | Adapted appended element offsets in the host input layout. The CPU rectangle fallback reads those same offsets. |
| 6325df7 | Shaders: print only the operands an instruction has in SP3 text | Not applicable | Disassembly pretty-printing only. |
| 988fd9a | translate: refuse a VICTORY_EMIT_DUMP directory inside the repo | Not applicable | Changes output-directory policy for the offline translator. |
| 350f91c | Formats: reject multisampled linear texture layouts | Useful | Adapted validation for multisampled linear layouts. The layout calculator reserves storage for only one linear sample. |
| 12eba49 | Shaders: fix whole-wave EXEC inline constant sign-extension and prefix lane validation | Already in ours | The pin contains the EXEC constant decoding, prefix checks and synthetic wave tests in emit/wave.rs. |
| 43e2ca9 | Show on-disk file size in victory inspect | Not applicable | A CLI inspection field has no effect on execution. |
| 6526965 | GPU: find a list in the DRED breadcrumbs by its name, not only its pointer | Not applicable | Superseded within upstream by the debug-layer name matching in 52d528b. DRED diagnostics run after device loss. |
| e34cb53 | GPU: keep finite (min_lod, min_lod) in CreateSamplerX when mip filter is none | Useful | D3D12 correctness group. Final sampler LOD rule for mip-none must accompany e6fe8ec. |
| 8a358e3 | GPU: find the lists in the DRED breadcrumbs by position when the debug layer hides them | Not applicable | Positional DRED identification was removed in 52d528b. |
| 52d528b | GPU: match a breadcrumb node's name followed by the debug layer's note; drop the position fallback | Not applicable | Improves breadcrumb labeling after loss. Does not address the cause of device loss. |
| 882664e | GPU: keep the frame wait's tick for the diagnostics log | Not applicable | Diagnostic companion to the upstream frame tick change, with different timing instrumentation here. |
| efe102e | Handle unsupported display deferrals in the HLE test | Not applicable | Changes a test for hosts without display deferral support. Current local display tests already pass. |
| 7216ab7 | HLE: implement the RPC runtime's GUID calls | Useful | Service correctness group. Adds a host RPC runtime module, GUID APIs and import routing as one unit. |
| a32eacd | fix(pipeline): retry collisions when creating cache staging files | Useful | Pipeline/cache group. Concurrent cache writers need unique staging names and cleanup on failed publication. |
| aaacc2d | docs(architecture): list BCPack as implemented in victory-formats | Not applicable | Documentation about an existing format decoder. |
| 64b80c3 | Formats: share immutable texture swizzle tables | Useful | Ported immutable tiled-layout swizzle tables. The opt-out builds a private table for each layout. |
| 121412b | loader(crash): verify unwind against known caller frames when RtlCaptureStackBackTrace returns empty | Not applicable | Loader unwind test for a host with an empty captured backtrace. No reported failure in this suite. |
| 3e98bd7 | GPU: seek directly to private data on final release | Useful | Adapted the ordered private-data lookup on final COM release. The old path visited unrelated objects while holding the global map lock. |
| 6d174ae | List successfully emitted shaders in offline translation scans | Not applicable | Lists output from an offline translation scan. |
| c430b1d | Shaders: tbuffer_load_format_d16 loads pack two 16-bit channels per VGPR | Useful | Recompiler group. Packed D16 loads must agree with per-VGPR channel layout in the buffer lowering. |
| c1c3281 | GPU: extend dynamic descriptor wraparound scan to prevent false exhaustion | Useful | Ported the bounded second search past the cursor in the dynamic heap. A false refusal can cost a whole draw and poison the negative result cache. |
| 3d71ad1 | Shaders: s_atc_probe's immediate is not a register | Useful | Recompiler group. The opcode's immediate must stay out of register dataflow and disassembly operands. |
| 072fde0 | Exercise a free run straddling the cursor on the actual host heap | Useful | Adapted the cursor-crossing and physical-end cases into a synthetic bitmap regression that runs without a graphics adapter. |
| c1be75f | Merge pull request #1 | Not applicable | Merge record. See the feature at 072fde0. |
| 131944d | PE: add synthetic unit tests for implib.rs and layout.rs | Not applicable | Pure test expansion for import-library generation and layout. These functions do not participate in the selected ports. |
| f821cb9 | GPU: pipelines of different names load from the library at the same time | Useful | Pipeline group. Parallel library loads must coordinate identical names with warmup and stand-in construction. |
| 0be58e6 | GPU: add synthetic unit tests for query.rs and signature.rs | Useful | Applied the original query heap and command-signature ABI tests to the worktree. No runtime dependency. |
| d0dd9fc | fix(pe): keep every name for a shared export ordinal | Useful | Ported PE export alias enumeration. A synthetic shared-ordinal test covers multiple names for one address. |
| 1aac20a | fix(loader): stage prepared images in per-writer files | Useful | Adapted per-writer staged image publication, including exclusive creation and cleanup after partial writes. |
| ca362a4 | fix(hle): read manifest fields past inline XML comments | Useful | Service correctness group. XML comments change field discovery and should be covered alongside quoted attributes. |
| 71e8866 | fix(hle): honor the requested executable on relaunch | Not applicable | Applies to relaunching a different executable from a multi-executable package. |
| eb8067f | fix(hle): keep apostrophes intact when splitting relaunch args | Not applicable | Applies to quoting a relaunch command line, outside the measured launch path. |
| f47ebd8 | feat(shader): lower buffer cache invalidations as no-ops | Already in ours | The fork contains the lowering and buffer_cache_invalidations_lower_as_no_ops regression. |
| 52f0a09 | policy: resolve a `..` in the not-yet-existing part of a path | Not applicable | Offline output-path policy, not used by the runtime. |
| fd44561 | Config: ignore an empty VICTORY_CONFIG | Not applicable | Configuration discovery when VICTORY_CONFIG is set to an empty string. The benchmark has a configured launch. |
| 60d2cf9 | Loader: a clwb in the last bytes of a page is emulated instead of faulting | Superseded | The first instruction handler decodes CLWB from the available readable bytes, including complete short instructions at a page end. |
| 246ecea | HLE: test that XGSuggestSwizzleMode picks linear for a 1D texture | Not applicable | Isolated test of an existing host service's suggested 1D swizzle mode. |
| 45ed8e3 | Shaders: hardware test for v_cvt_f16_u16 and v_cvt_f16_i16 | Useful | Recompiler group. Hardware conversion test belongs with the corrected exactly representable inputs in 8d67fa5. |
| f177aae | Shaders: cap BDD memo table and bail out early on overflow | Useful | Recompiler group. A bounded decision-diagram cache affects compile-time acceptance as well as translation time. |
| 8d67fa5 | Shaders: keep the v_cvt_f16_u16/i16 test to integers f16 holds exactly | Useful | Recompiler group. Corrects the reference inputs for 45ed8e3. |
| 534e1dd | CLI: add AVX2 pre-flight CPU check before executing titles | Not applicable | The measured machine meets the CPU requirements. This adds a launch-time diagnostic for another class of host. |
| 03a6f22 | XMA: gather frame payloads one byte at a time | Not applicable | Audio payload gathering on a decoder worker is outside the measured critical path. |
| 259ba31 | Shaders: negative-zero ordering test for the f16 min/max forms | Useful | Recompiler group. Signed-zero ordering tests belong with the floating-point lowering and host device harness. |
| 8610ffb | Shaders: hardware test for the integer v_min3/v_max3/v_med3 | Useful | Recompiler group. Hardware tests cover signed and unsigned three-input instruction lowering. |
| e3c9dd0 | GPU: guard zero/overflowing stride in expand_rects and add primitive.rs unit tests | Useful | Guarded empty or overflowing rectangle strides before chunk iteration in the CPU fallback. |
| ef0c3e9 | GPU: validate null args in create_deserializer to prevent d3d12 AV and add root.rs tests | Useful | D3D12 correctness group. Null root-deserializer arguments and interface outputs need coverage against both device layouts. |
| 41b2472 | Shaders: name the pick function type in the integer min3/max3/med3 test | Not applicable | Test-only type alias in the upstream integer min/max suite. |
| 42961e8 | GPU: log the TDR settings, and warn when a GPU hang would freeze the PC | Not applicable | Registry settings in the diagnostic log do not fix a device hang. |
| a7847a5 | HLE: constrain the title before suspending it | Useful | Service correctness group. Lifecycle callback and suspension ordering must be reviewed together. |
| 098a26a | README: how to install Rust on Windows (#1087) | Not applicable | Host toolchain installation instructions only. |
| 44ce067 | XAsync: canceling a call that already completed leaves it alone, as documented | Useful | Service correctness group. Cancel races with completion in the same XAsync state machine as d01b170. |
| a14a545 | Merge remote-tracking branch 'origin/main' into fix/clzero-loops-merge | Not applicable | Synchronizes the development branch with the shared history at ffe310e. |
| 514927d | Loader: tests for the clzero loop edge cases | Not applicable | The tests call the upstream combined fallback handler. The pin dispatches to console_insn first and tests prefix and register forms in console_insn_decode. |
| 47c00d5 | GPU diagnostics: create missing capture output directories | Not applicable | Capture output directory setup affects explicitly requested image capture. |
| 142ec8d | Deduplicate image paths in victory census | Not applicable | Deduplicates user-supplied paths to the offline census tool. |
| 344704e | Shaders: lower v_mul_hi_u32_u24 and v_mul_hi_i32_i24 | Useful | Recompiler group. Adds signed and unsigned 24-bit high multiply lowering. |
| 5d6889e | Shaders: lower v_sad_u8, v_sad_hi_u8, v_sad_u16, v_sad_u32 and v_msad_u8 | Useful | Recompiler group. Adds packed absolute-difference operations in the shared VALU lowerer. |
| 643fc6d | HLE: avoid misaligned f32 output slice in XApu | Useful | Service correctness group. Unaligned audio output needs byte-safe writes and its synthetic ABI checks. |
| 64a33ec | XNetworking: wire UDP port change callbacks and UTF-16 security queries | Useful | Service correctness group. UDP callback lifetimes and wide security queries share service registration. |
| 67c4f53 | victory-shader: resolve Structured::other and constant masks in v_cndmask_b32 | Useful | Recompiler group. Structured mask classification overlaps the parked-lane and paired-wave work. |
| 59035de | GPU: grow per-list upload scratch on demand | Superseded | The pin's scratch pool starts small, grows and recycles chunks. Patch 0012 also pools staging uploads. |
| 4f54c47 | HLE: complete XPackage progress callback unregistration | Useful | Service correctness group. Package progress callback unregistration must synchronize with delivery. |
| 1ed26e5 | Shaders: fold conditional branches to their fallthrough | Useful | Recompiler group. Fallthrough branch folding changes the CFG used by EXEC tracking. |
| 100de0e | Shader tests: a skipped device says why it was skipped | Not applicable | Adds test skip diagnostics without changing generated shader output. |
| 32eb934 | docs: explain Windows shader-test validator prerequisite | Not applicable | Documents a shader validator test prerequisite. |
| 5c44562 | Shaders: compress replaced phi chains in remove_trivial_phis | Useful | Recompiler group. Path compression in trivial phi removal affects compile time and nested control flow. |
| 645391b | Shaders: a normalized buffer format's ends load as exactly 1.0 and -1.0 | Useful | Recompiler group. Bring the final quotient rounding rule da82e44 together with endpoint regressions. |
| 97c8783 | GPU: keep A8_UNORM out of the R8 typeless family | Useful | Adapted the A8 typeless family rule. An alpha-only resource can no longer qualify for an R8 cast when enabled. |
| 78c1267 | Shaders: lower v_cvt_off_f32_i4 | Useful | Recompiler group. Adds the biased integer-to-float conversion to VALU lowering. |
| e45bacd | Shaders: preserve mip point filtering in anisotropic static samplers | Useful | Recompiler/pipeline group. Static sampler mip filtering needs the host capability check from 23fc8a5. |
| 4e155a3 | Shaders: do not parse VS inputs when reading mesh shader stage I/O | Useful | Recompiler group. Mesh-stage I/O must be parsed independently of VS inputs and validated with mesh signatures. |
| 2b13ae4 | HLE clears the result of blocked address lookups | Useful | Service correctness group. Failed offline address lookups must clear their result pointer. |
| 23fc8a5 | Shaders: gate anisotropic point-mip host filters | Useful | Pipeline group. Gate point-mip anisotropy on host support across static and dynamic samplers. |
| 7dfa86f | GPU: honor anisotropic point-mip device support | Useful | D3D12 correctness group. Dynamic sampler counterpart to the static filter capability changes. |
| c3a04e2 | Shaders: lower scalar quadmask instructions | Useful | Recompiler group. Scalar quadmask lowering and native-wave assumptions need the hardware case from e3fe5bf. |
| e3fe5bf | Test scalar quadmask lowering on D3D12 | Useful | Recompiler group. D3D12 readback regression for c3a04e2. |
| a7636dd | HLE: allow an explicit maximum title window size | Not applicable | A maximum host window size would change the resolution used to compare heavy frames. |
| e52777a | GPU list: bind initial pipeline state in create_list and create_list_x | Already in ours | Both creation entry points already pass the initial pipeline to new_list, which calls State::set_pipeline. |
| 6f36cdf | GPU: interpolate normalized rectangle vertex attributes | Useful | Adapted scalar and two-channel UNORM and signed normalized attribute handling for rectangle fallback draws. |
| be3202d | Shaders: fold empty loop latches containing only dead phis | Useful | Recompiler group. Empty-loop folding depends on phi liveness in the fork's DXIL optimization passes. |
| 711d651 | Shaders: precompute MD5 round constants and avoid per-block Vec in md5_block | Useful | Adapted fixed MD5 constants and stack words in the shader container writer. Synthetic blocks check digest equivalence. |
| 1e923ad | Shaders: set WAVE_OPS feature flag when lane_bit emits waveGetLaneIndex | Already in ours | The WAVE_OPS flag is set in the fork's lane-bit lowering. |
| 2286e03 | Shaders: key immediate sampler slots by descriptor words and comparison mode | Useful | Recompiler group. Immediate sampler identity must include comparison mode as well as descriptor words. |
| 4ab20fc | GPU: the indirect budget is charged in record order and no longer wraps | Useful | Indirect group. The local WARP tests accept either charging order and 0041 charges amplification work on the card. Upstream imposes record order and prevents wrap. Compare the budget semantics before integrating. |
| 9ccb83c | HLE: guard dialog commands before initialization | Useful | Service correctness group. Dialog commands can arrive during window initialization. |
| 274f698 | Loader: guard clwb_loop shortcut against u64 overflow and wraparound | Superseded | The active console_insn handler patches individual CLWB hints and lets the CPU execute the loop arithmetic. The upstream fix targets the secondary handler. |
| f078008 | Formats: test texture layouts against addrlib's own sizes and addresses | Useful | Texture group. Reference layout tests belong alongside the local page, tail and staging-layout changes. |
| bbdc3ca | GPU: fences answer for ID3D12Fence1 as documented | Useful | Adapted Fence1 discovery for creation and QueryInterface. The creation-flags method already exists. |
| 3f6910a | CLI: propagate the title entry point exit status | Not applicable | CLI exit status propagation does not change guest execution or frame delivery. |
| e9cb14a | run: stop with a clear message when the title's own DLLs are missing | Not applicable | Startup diagnostic for incomplete packages. The measured run reaches steady-state rendering. |
| f096774 | GPU: throttle failed swap-chain creation and resize | Useful | Presentation group. Retry throttling must coordinate the asynchronous present worker with resize and device loss. |
| be824d9 | GPU: serialize pipeline library atomic writes with unique temp files | Useful | Pipeline group. Keep library publication and unique temp-file cleanup under the same concurrency contract. |
| eb7cc2d | flush a kept save blob that had to be copied | Useful | Service correctness group. Durable save writes span copy, flush and metadata publication. |
| 4fc9d70 | GPU: keep Staging::detile inside the staging buffer and harden committed() | Useful | Texture group. Size and committed-range checks affect every staging detile and sparse mapping. |
| 9145d9d | HLE: show title windows through a stand-in that always reads its messages | Useful | Service/window group. Introduces a separate message pump and standby window lifecycle, overlapping the local window patch 0043. |
| 6493dd9 | Shader tests: require a device readback in every waterfall test | Useful | Recompiler group. Waterfall readback tests need the fork's device-skip and wave-width conventions. |
| 8d3a025 | gpu(list): log initial pipeline bound on reset/reset_x for parity with create_list | Not applicable | Diagnostic tracing for reset binds. Reset already calls state.reset(pipeline). |
| 40f30be | Shaders: GFX10 FLAT offsets ignore bit 11 | Useful | Recompiler group. Correct FLAT offset width affects shader memory addressing. |
| c0228d9 | victory-shader: mask mesh primitive vertex indices to 9 bits (drop edge flags) | Useful | Recompiler correctness branch already carries the mesh-index mask. Avoid integrating it twice. |
| d09f3aa | DirectStorage: read a page list naming page 0 again before failing its request | Useful | Service correctness group. DirectStorage late page lists overlap that branch's request-time mapping work. |
| 0b35b7f | Shaders and GPU: FidelityFX on host and console paths, 64-bit image atomics, the BVH intersection, lane masks, bindless strides | Useful | Ray-tracing and recompiler groups. Image atomics, BVH intersection, bindless strides and host upscaling span two runtimes. The pin already implements a separate BVH path. |
| a7fc47a | Shaders: a phi edge is live only if a lane can take it to a read | Useful | Recompiler group. Phi liveness depends on divergent edges and the fork's lane-mask representation. |
| 13091f6 | Shaders: pixel and mesh I/O with reused registers, 16-bit packing and signature rows | Useful | Recompiler group. Packed and reused pixel/mesh registers affect signature linking. Compare with patch 0047. |
| 27c3dc9 | Shaders and GPU: title memory at raw addresses through a page table | Useful | Recompiler/coherence groups. A GPU page table for raw addresses needs residency, updates, shader lowering and copy-back as a unit. |
| 8af0472 | Shaders: lane masks spilled and combined in compute programs | Useful | Wave64 group. Spilled compute lane masks share representation with the paired-wave implementation. |
| 4051149 | GPU: null views for title buffer views that are empty or run past their copy | Useful | Descriptor group. Empty and overlong buffer views must be resolved against the actual host copy at bind time. |
| 328eec1 | Shader tests: instruction semantics and control flow against the console's float mode | Useful | Recompiler group. Differential tests require upstream's floating-point mode setup and control-flow fixtures. |
| 75677cb | Shaders: buffer sizes read from descriptors, and size-only waterfall loops | Useful | Descriptor/recompiler groups. Descriptor size reads and waterfall simplification must agree with host metadata. |
| 0f63757 | GPU: root views of programs that use title memory go through the page table | Useful | Recompiler/coherence groups. Follows the raw-address page-table architecture in 27c3dc9. |
| 77053a0 | Shaders: accuracy fixes for discards, f16 rounding, denormals, med3 and signed zeros | Useful | Recompiler group. Several changes to discard, denormal and signed-zero semantics share emit and device readback tests. |
| 4f57f46 | Shaders: cube samples with gradients take their level from the gradients | Useful | Recompiler group. Cube gradients affect explicit level selection and need the sampling tests on its branch. |
| c75fdba | GPU: views the shaders pick from the heap, and one change journal for title memory | Useful | Descriptor/coherence groups. Indirect heap lookup plus one shared change journal crosses recording, submission and write tracking. |
| d71c388 | GPU and shaders: bounded root views, and a guard against texture descriptors read as buffers | Useful | Descriptor/recompiler groups. The fork already bounds root SRVs. Compare the remaining buffer-versus-texture discriminator on both sides of the shader interface. |
| 683ad7a | GPU and shaders: rectangle lists through a geometry shader, table constants read in place | Superseded | The pin already completes rectangles in a geometry stage. Patch 0038 binds table constants to the host copy on the GPU. |
| 7389303 | Game formats: effects, techniques and passes of the shader database; shader-info --effects | Not applicable | Offline shader database inspection. There is no pack reader in the runtime path selected here. |
| 286e33f | Game formats: the shader pack; shader-info lists packs | Useful | Preload group. Shader pack parsing is a prerequisite for offline PSO prebuild in 9c58a1c. |
| 532cb7c | Diagnostics: VICTORY_POKE writes title bytes twice a second | Not applicable | A periodic diagnostic writer changes guest bytes and cannot be used to compare normal rendering. |
| c47b529 | Shader warm-up (translate --cache), a cache keyed by the translator, pipeline queue fixes | Useful | Pipeline group. Compare translator-keyed caches and work queue changes with 0026 and the warmup branch. |
| 819f622 | translate: progress counts finished shaders, not ones handed out | Not applicable | Changes progress reporting for an offline translator run. |
| e0034c2 | Shaders: register-relative moves reach only what their index can | Useful | Recompiler group. Register-relative move bounds affect which values survive generated loops. |
| 13bbd6f | GPU: one host texture per texture memory, and writable twins for depth textures | Already in ours | The fork shares host textures by backing memory and routes writable depth through a twin. The local copy path already records twin use. |
| 224e786 | GPU tests: the frame pacing test tolerates a busy host | Not applicable | Relaxes timing tolerance for a frame pacing test. No matching failure in the tested baseline. |
| 86a5ed6 | GPU: views off their buffer's elements get their own copy; heap records copied as bytes are mirrored | Already in ours | Fork a26859f already mirrors byte-copied view records and gives misaligned views their own backing bytes. |
| 6d021fe | GPU: world frame cost | Useful | Submit/descriptor groups. Large mixed cost reduction. Local patches 0013, 0029, 0038 and 0045 already handle refresh, mirror sync and direct constants. Compare remaining pieces per stage. |
| bacb06a | GPU: a heap slot holds its view's own copy | Useful | Descriptor group. Upstream ties copies to heap-slot lifetime. Compare against a26859f and the fork's per-draw tables before changing which object keeps a copy alive. |
| 9c58a1c | Pipeline pre-build from the shader pack (translate --cache --pipelines); mesh pipelines load from the library | Useful | Preload group. Pack-based PSO prebuild depends on 286e33f and mesh library loading. Compare hit rates with warmup. |
| 7000573 | Diagnostics: title memory peeks, slot states, small buffers and 1D/3D read-back in listed frames | Not applicable | Additional deep inspection for explicitly traced frames. No specific slot or readback from this capture requires it. |
| 70564ae | README: status of 2026-11-24 | Not applicable | Historical status report. |
| 13185f5 | GPU: GetDeviceRemovedReason reports the host device's state instead of E_NOTIMPL | Useful | Ported host removal status at slot 37. The disabled device table retains its original missing-method entry. |
| e29391f | Shaders: honor op_sel, omod and clamp on v_interp_p2_f16 | Useful | Recompiler group. Packed f16 interpolants need operand selection, output modifiers and clamping together. |
| 146dfef | gfx10: resolve the opcode name once, at decode time | Useful | Recompiler group. Decode-time mnemonic lookup changes the instruction representation consumed by the translator. |
| b8dbcba | XGameSave: a stray or unreadable version folder never costs the save | Useful | Service correctness group. Save recovery and obsolete-version cleanup must agree on publication and durability. |
| 0601955 | Shaders: a helper lane's v_readfirstlane reads its own value | Useful | Recompiler group. Helper-lane readfirstlane behavior needs the pixel-stage wave tests. |
| ed0dbfd | GPU: copy title writes under ranges a submission's shaders write | Already in ours | Shadow::merge_under_writes already preserves guest writes under in-flight ranges. Patch 0042 records the cases where the merge gives up. |
| 4e60ea3 | victory-gpu: drop an indirect call before its clamp, not after | Already in ours | The local indirect path looks up the host pipeline and records a drop before resolving or refreshing the argument buffer or dispatching a clamp. |
| b5ac27f | Merge pull request #115 | Not applicable | Merge record. See the feature at b1bdd89. |
| f8c7498 | Merge pull request #122 | Not applicable | Merge record. See the feature at d01b170. |
| ed6429e | Merge pull request #132 | Not applicable | Merge record. See the feature at b9ff219. |
| 95191f8 | Merge pull request #186 | Not applicable | Merge record. See the feature at 3c7d845. |
| 5d1fbac | Merge pull request #207 | Not applicable | Merge record. See the feature at 11ca3ce. |
| 65aef25 | Merge pull request #238 | Not applicable | Merge record. See the feature at 534e1dd. |
| c7f1ae3 | Merge pull request #335 | Not applicable | Merge record. See the feature at e3bef08. |
| a222fe3 | Merge pull request #357 | Not applicable | Merge record. See the feature at c6959ac. |
| 2d8cac2 | Merge pull request #476 | Not applicable | Merge record. See the feature at a3f89ed. |
| a34ac58 | Merge pull request #538 | Not applicable | Merge record. See the feature at 65062e1. |
| e82a6ad | Merge pull request #615 | Not applicable | Merge record. See the feature at a4974e4. |
| ff15f98 | Merge pull request #636 | Not applicable | Merge record. See the feature at 7216ab7. |
| 417e3f9 | Merge pull request #664 | Not applicable | Merge record. See the feature at 988fd9a. |
| 644ebeb | Merge pull request #665 | Not applicable | Merge record. See the feature at e950e81. |
| b00c14a | Merge pull request #1000 | Not applicable | Merge record. See the feature at f821cb9. |
| 76bcd2b | Merge pull request #1080 | Not applicable | Merge record. See the feature at a7847a5. |
| 88de5fc | Merge pull request #1611 | Not applicable | Merge record. See the feature at e9cb14a. |
| d4ef9eb | Merge pull request #1677 | Not applicable | Merge record. See the feature at 9145d9d. |
| 26e03c3 | Merge pull request #1729 | Not applicable | Merge record. See the feature at b8dbcba. |
| 4bf5100 | Merge pull request #12 | Not applicable | Merge record. See the feature at c63262f. |
| 1117251 | Merge pull request #52 | Not applicable | Merge record. See the feature at 628b95d. |
| c5b83b9 | Merge pull request #219 | Not applicable | Merge record. See the feature at e57e76a. |
| 7b96711 | Merge pull request #247 | Not applicable | Merge record. See the feature at 5cff000. |
| c8cbe8a | Merge pull request #353 | Not applicable | Merge record. See the feature at e6ebec9. |
| 8595259 | Merge pull request #359 | Not applicable | Merge record. See the feature at d549b63. |
| 897efe0 | Merge pull request #370 | Not applicable | Merge record. See the feature at 936e5e5. |
| ac2525d | Merge pull request #373 | Not applicable | Merge record. See the feature at c000581. |
| e686512 | Merge pull request #455 | Not applicable | Merge record. See the feature at eb8067f. |
| 43b7c08 | Merge pull request #474 | Not applicable | Merge record. See the feature at e34cb53. |
| ce2d91f | Merge pull request #475 | Not applicable | Merge record. See the feature at 2c22e2b. |
| 2c67b09 | Merge pull request #480 | Not applicable | Merge record. See the feature at 75fe6c0. |
| 805317b | Merge pull request #487 | Not applicable | Merge record. See the feature at ca362a4. |
| 54e7844 | Merge pull request #504 | Not applicable | Merge record. See the feature at b956320. |
| 648a9ff | Merge pull request #555 | Not applicable | Merge record. See the feature at d0dd9fc. |
| 2fb667b | Merge pull request #617 | Not applicable | Merge record. See the feature at 81b81df. |
| 9bd146d | Merge pull request #618 | Not applicable | Merge record. See the feature at 691236d. |
| dcd24fe | Merge pull request #649 | Not applicable | Merge record. See the feature at 43e2ca9. |
| 9b6d1c0 | Merge pull request #957 | Not applicable | Merge record. See the feature at 246ecea. |
| 05394a6 | Merge pull request #993 | Not applicable | Merge record. See the feature at 64b80c3. |
| e9f43ed | Merge pull request #1082 | Not applicable | Merge record. See the feature at 42961e8. |
| 357a8bc | Merge pull request #1091 | Not applicable | Merge record. See the feature at 098a26a. |
| 6d06afb | Merge pull request #1098 | Not applicable | Merge record. See the feature at 44ce067. |
| d73d8c9 | Merge pull request #1109 | Not applicable | Merge record. See the feature at a32eacd. |
| 7582940 | Merge pull request #1161 | Not applicable | Merge record. See the feature at aaacc2d. |
| fc438a2 | Merge pull request #1171 | Not applicable | Merge record. See the feature at 131944d. |
| 69bab30 | Merge pull request #1173 | Not applicable | Merge record. See the feature at 0be58e6. |
| 0fcbee4 | Merge pull request #1177 | Not applicable | Merge record. See the feature at 142ec8d. |
| 432b110 | Merge pull request #1204 | Not applicable | Merge record. See the feature at e3c9dd0. |
| 02deab1 | Merge pull request #1206 | Not applicable | Merge record. See the feature at ef0c3e9. |
| 5c4f2e9 | Merge pull request #1248 | Not applicable | Merge record. See the feature at 64a33ec. |
| 6355117 | Merge pull request #1274 | Not applicable | Merge record. See the feature at 4f54c47. |
| eb17946 | Merge pull request #1299 | Not applicable | Merge record. See the feature at 32eb934. |
| 05f988c | Merge pull request #1390 | Not applicable | Merge record. See the feature at 97c8783. |
| 7db1e07 | Merge pull request #1411 | Not applicable | Merge record. See the feature at 2b13ae4. |
| 12ee4a1 | Merge pull request #1453 | Not applicable | Merge record. See the feature at 6f36cdf. |
| a985b9d | Merge pull request #1455 | Not applicable | Merge record. See the feature at a7636dd. |
| e011e88 | Merge pull request #1515 | Not applicable | Merge record. See the feature at 9ccb83c. |
| 17de445 | Merge pull request #1539 | Not applicable | Merge record. See the feature at 274f698. |
| ebcd90f | Merge pull request #1592 | Not applicable | Merge record. See the feature at 6d174ae. |
| cbb3431 | Merge pull request #1594 | Not applicable | Merge record. See the feature at 3f6910a. |
| 711e930 | Merge pull request #1658 | Not applicable | Merge record. See the feature at 4fc9d70. |
| a9d233f | Merge pull request #1660 | Not applicable | Merge record. See the feature at be824d9. |
| f69e487 | Merge pull request #1667 | Not applicable | Merge record. See the feature at eb7cc2d. |
| d6e00a8 | Merge pull request #133 | Not applicable | Merge record. See the feature at 01ae55a. |
| 2a45e75 | Merge pull request #194 | Not applicable | Merge record. See the feature at 985d8d4. |
| 280e484 | Merge pull request #941 | Not applicable | Merge record. See the feature at 60d2cf9. |
| 4d140e2 | Merge pull request #1718 | Not applicable | Merge record. See the feature at c0228d9. |
| 12fa535 | Merge pull request #96 | Not applicable | Merge record. See the feature at ba42db6. |
| c813777 | Merge pull request #126 | Not applicable | Merge record. See the feature at e5b7159. |
| 46bde7f | Merge pull request #169 | Not applicable | Merge record. See the feature at 645391b. |
| 71ac890 | Merge pull request #213 | Not applicable | Merge record. See the feature at 7f00a62. |
| 15de8f9 | Merge pull request #214 | Not applicable | Merge record. See the feature at 514927d. |
| a9d6f17 | Merge pull request #322 | Not applicable | Merge record. See the feature at b58d198. |
| 1134418 | Merge pull request #447 | Not applicable | Merge record. See the feature at 7d04943. |
| 5c0ebce | Merge pull request #646 | Not applicable | Merge record. See the feature at 12eba49. |
| ea48bd3 | Merge pull request #669 | Not applicable | Merge record. See the feature at 52d528b. |
| 0cffc54 | Merge pull request #937 | Not applicable | Merge record. See the feature at 52f0a09. |
| 0299812 | Merge pull request #990 | Not applicable | Merge record. See the feature at 8d67fa5. |
| 181825d | Merge pull request #1146 | Not applicable | Merge record. See the feature at 47c00d5. |
| 1c16afd | Merge pull request #1214 | Not applicable | Merge record. See the feature at 643fc6d. |
| aa83e3f | Merge pull request #1246 | Not applicable | Merge record. See the feature at 59035de. |
| 7ae5e17 | Merge pull request #1474 | Not applicable | Merge record. See the feature at 1e923ad. |
| 41b11b2 | Merge pull request #1477 | Not applicable | Merge record. See the feature at 2286e03. |
| fe88382 | Merge pull request #1510 | Not applicable | Merge record. See the feature at 4ab20fc. |
| 8fb0b28 | Merge pull request #1549 | Not applicable | Merge record. See the feature at f078008. |
| 94856e2 | Merge pull request #1635 | Not applicable | Merge record. See the feature at 4e60ea3. |
| 8479c18 | Merge pull request #1726 | Not applicable | Merge record. See the feature at d09f3aa. |
| aa1bf51 | Merge pull request #153 | Not applicable | Merge record. See the feature at 3e98bd7. |
| 256d7b9 | Merge pull request #156 | Not applicable | Merge record. See the feature at 344704e. |
| 83211b9 | Merge pull request #162 | Not applicable | Merge record. See the feature at 5d6889e. |
| 7d99f63 | Merge pull request #172 | Not applicable | Merge record. See the feature at 4eb0898. |
| 8a772ea | Merge pull request #174 | Not applicable | Merge record. See the feature at 882664e. |
| 338808e | Merge pull request #183 | Not applicable | Merge record. See the feature at d4d1822. |
| a3169a1 | Merge pull request #197 | Not applicable | Merge record. See the feature at 64484b4. |
| 3dce4ba | Merge pull request #221 | Not applicable | Merge record. See the feature at acdcc41. |
| 38011a0 | Merge pull request #270 | Not applicable | Merge record. See the feature at 92f2c60. |
| 7448632 | Merge pull request #288 | Not applicable | Merge record. See the feature at f177aae. |
| 32b9fbf | Merge pull request #351 | Not applicable | Merge record. See the feature at 93a24b7. |
| e8e494c | Merge pull request #361 | Not applicable | Merge record. See the feature at 3f5eff5. |
| d763582 | Merge pull request #363 | Not applicable | Merge record. See the feature at fd44561. |
| 9ce9976 | Merge pull request #368 | Not applicable | Merge record. See the feature at f47ebd8. |
| 13de0cc | Merge pull request #536 | Not applicable | Merge record. See the feature at 7a5d85e. |
| e56919d | Merge pull request #553 | Not applicable | Merge record. See the feature at b796c8b. |
| 40f5553 | Merge pull request #560 | Not applicable | Merge record. See the feature at 1aac20a. |
| 8bf20ad | Merge pull request #603 | Not applicable | Merge record. See the feature at 705a2f6. |
| 9574cee | Merge pull request #629 | Not applicable | Merge record. See the feature at 350f91c. |
| 02fcf5b | Merge pull request #661 | Not applicable | Merge record. See the feature at 6325df7. |
| c6c244d | Merge pull request #974 | Not applicable | Merge record. See the feature at c1be75f. |
| 924457e | Merge pull request #1016 | Not applicable | Merge record. See the feature at 03a6f22. |
| 6e6004c | Merge pull request #1021 | Not applicable | Merge record. See the feature at 3d71ad1. |
| fdce5f5 | Merge pull request #1050 | Not applicable | Merge record. See the feature at 259ba31. |
| 19469bd | Merge pull request #1052 | Not applicable | Merge record. See the feature at 41b2472. |
| ef65f88 | Merge pull request #1125 | Not applicable | Merge record. See the feature at efe102e. |
| e2a80cf | Merge pull request #1159 | Not applicable | Merge record. See the feature at 121412b. |
| 82494a9 | Merge pull request #1180 | Not applicable | Merge record. See the feature at 146dfef. |
| 6160c9e | Merge pull request #1221 | Not applicable | Merge record. See the feature at 67c4f53. |
| 825c045 | Merge pull request #1282 | Not applicable | Merge record. See the feature at 1ed26e5. |
| 87f01ff | Merge pull request #1290 | Not applicable | Merge record. See the feature at 100de0e. |
| 170ab23 | Merge pull request #1337 | Not applicable | Merge record. See the feature at 7dfa86f. |
| 21b4175 | Merge pull request #1366 | Not applicable | Merge record. See the feature at 4e155a3. |
| 67bcf47 | Merge pull request #1371 | Not applicable | Merge record. See the feature at 5c44562. |
| 5e3bc84 | Merge pull request #1391 | Not applicable | Merge record. See the feature at 78c1267. |
| 95f6c6e | Merge pull request #1443 | Not applicable | Merge record. See the feature at e3fe5bf. |
| 24effa8 | Merge pull request #1446 | Not applicable | Merge record. See the feature at 8d3a025. |
| 520fefc | Merge pull request #1471 | Not applicable | Merge record. See the feature at be3202d. |
| f73e8e1 | Merge pull request #1476 | Not applicable | Merge record. See the feature at 711d651. |
| 3c47fd4 | Merge pull request #1562 | Not applicable | Merge record. See the feature at bbdc3ca. |
| 13d1237 | Merge pull request #1626 | Not applicable | Merge record. See the feature at f096774. |
| 3688adc | Merge pull request #1636 | Not applicable | Merge record. See the feature at 40f30be. |
| 1bde03c | Merge pull request #1695 | Not applicable | Merge record. See the feature at 6493dd9. |
| fcdefcf | Merge pull request #1322 | Not applicable | Merge record. See the feature at ed0dbfd. |
| 5fa5de3 | Merge pull request #1733 | Not applicable | Merge record. See the feature at 0601955. |
| 4436fca | Merge pull request #1696 | Not applicable | Merge record. See the feature at e29391f. |
| 4118f33 | Merge pull request #928 | Not applicable | Merge record. See the feature at 13185f5. |
| 42363ab | Tests: hardware-only shader checks skip on a machine without a GPU | Already in ours | Fork 1684989 and 3930989 already skip hardware-only checks on unsupported test hosts. |
| ccfb029 | Tests: a machine with no GPU has no hardware adapter to check shaders on | Already in ours | Fork 7b0f736 checks the software adapter's description when choosing a hardware test device. |
| cd94d35 | GPU: shaders mark the pages they write through views; copy-backs compare only those | Useful | Relay/recompiler groups. Shader write-mark instrumentation and page-granular copy-back require matching shader caches and host buffers. |
| 940d32f | GPU: a marked page's copy-back does not save the page for its own copy | Useful | Relay group. Companion to cd94d35, with a change to snapshot ownership during copy-back. |
| bd9b273 | GPU: a draw dropped because a root view cannot be bound says why | Useful | Pipeline/descriptor groups. Root-view drop reasons complement 0047 when diagnosing actual missing draws. |
| 1365fe1 | GPU: diagnostics for the frame's cost: heap sync time, video memory, GPU time per pipeline | Superseded | Local workload and frame wait counters plus GPU timing cover heap costs, memory and per-pipeline time (0006, 0041, 0046). |
| 1953339 | GPU: a submission skips host copies the title has not written since they took it | Superseded | Submission refresh and reservation-level write-watch (0013, 0045) already avoid many unchanged reads. Upstream's per-copy dirty flag belongs to a different tracking architecture. |
| 3e10052 | GPU: D3D12 objects freed after a submission are released on a thread of their own | Useful | Submit group. Moving COM releases off the submission thread needs bounded retention and fence ownership for completed submissions. |
| 0086ccb | GPU: traced shaders' indirect records are logged in every frame | Not applicable | Indirect record dumps on every traced frame change capture volume. The current run already counts the clamp kinds. |
| eddde73 | HLE: DirectStorage reads into physical pages wait for the title's Submit | Useful | Service correctness group. Physical-page DirectStorage reads must be evaluated at submission against this fork's page alias lifetimes. |
| f1c55a8 | GPU: a placed texture's slot left null is not made again at every heap sync | Useful | Descriptor group. Caching a null placed view requires invalidation when the descriptor or texture becomes available. |
| 67e330a | GPU: process memory and a host texture census in the statistics | Superseded | Fork 8dd30ab and the frame/copy statistics already record process and texture memory used in runs. |
| a7381ed | GPU: host textures made on first use | Useful | Texture group. Lazy creation changes how descriptor generation, eviction and first-use uploads synchronize. |
| 5622159 | GPU: debug lines for sampled texture uploads | Not applicable | Debug output for sampled uploads. Use existing per-stage counters for timing. |
| 5cb63d8 | GPU: texture to buffer copies read back into the title's buffer | Already in ours | list::read_back already transfers a texture to a placed guest buffer including its destination row pitch. |
| 97721d1 | GPU: the title memory read to detile a tracked texture is not kept | Useful | Texture group. Discarding detile source storage must preserve the refresh and replay behavior of pooled staging buffers. |
| 7433988 | GPU: 8-bit RGBA dumps log their alpha range and zero counts | Not applicable | RGBA dump statistics affect explicit captures only. |
| 98c654b | GPU: the console's 16-bit depth with stencil is float depth on the host | Useful | Texture group. Host depth format, depth/stencil views and writable twins must agree on plane representation. |
| 2d4aa55 | GPU: the snapshot pool keeps at most 512 MiB | Useful | Relay group. Snapshot pooling trades RAM for repeated allocation. Size alongside the local card-compare and small-read pools. |
| 2a93ff0 | GPU: our heap bytes and the process heaps in the memory line (diagnostics) | Not applicable | Adds heap census instrumentation. Upstream later removed the expensive process-heap walk in ed10c27. |
| 280b150 | GPU: change tracking guards only the runtime's memory, region by region | Useful | Relay/coherence group. Upstream limits guard registration by owned memory region. Compare ownership and overlapping views with watch::track, scoped remaps (0037) and reservation-level write-watch (0045). |
| f809521 | GPU: a heap sync looks at a page's slots under one lock and makes only the changed ones | Useful | Descriptor group. Hold one lock per changed page when synchronizing mirrors. Local 0029 fingerprints and descriptor work use a different sync layout. |
| cc0e153 | GPU: change trackers keep their "written since taken" flag in one dense bitmap | Useful | Relay group. Dense per-copy dirty bitmaps must share invalidation and page ownership with the local journal. |
| ed887f2 | GPU: diagnostics: copies redone piece by piece are timed; GPU timing keys keep the pipeline | Superseded | The fork counts its own piecewise copy work and per-pipeline GPU time in diagnostics. |
| 6cc6aa3 | GPU: copies of single views share large buffers | Useful | Descriptor/relay groups. Sharing large buffers between single views changes offset, lifetime and dirty-range accounting. |
| c4e022d | GPU: diagnostics: title threads' wait for the heaps' lock; host copies in video memory | Useful | Descriptor group. Lock-wait diagnostics are useful with the corresponding changed-page synchronization and per-draw binding measurements. |
| 2922141 | GPU: a heap's merged view ranges are kept current, not rebuilt whole after each change | Useful | Descriptor group. Maintain merged live-view ranges incrementally. The local texture heap allocator tracks free allocation ranges, a different data structure. Reconcile with the changed-record mirror. |
| ac7ffc0 | GPU: large heap allocations name their caller; list upload memory in the memory line (diagnostics) | Not applicable | Diagnostic heap allocation provenance and memory census add no direct runtime fix. |
| 23a4b62 | GPU: a texture is uploaded once per submission, and only when its bytes changed | Superseded | Local 0013, 0027 and 0037 track texture writes and refresh around submission, coherence and remapping. |
| a2c50db | GPU: a drop counted by its reason names the pipeline's shaders | Useful | Pipeline group. Correlate shader IDs with visible drop reasons alongside the existing 0047 pipeline refusal log. |
| 59c78bb | Shaders: descriptors joined by nested control flow | Not applicable | Intermediate nested-descriptor implementation explicitly reverted by fbac5bb. Evaluate the final version at 54ab406. |
| ff47463 | Shaders: a descriptor no lane brings along an edge needs no source there | Useful | Recompiler group. Dead lane edges must agree with nested descriptor joins in the final lowering. |
| e8e9f0c | Shaders: a lane mask joined with data at a uniform join is its number | Useful | Wave64 group. Uniform joins between scalar data and a lane mask change how a paired wave represents the result. |
| fbac5bb | Shaders: descriptors joined by nested control flow stay refused for now | Not applicable | Reverts the first nested-descriptor attempt. Use the corrected final approach in 54ab406. |
| f342dcf | Shaders: inputs a counted v_movreld loop overwrites first need no value | Useful | Recompiler group. Inputs killed by indexed VGPR writes need dataflow proofs before removing initial values. |
| eed1670 | GPU: a listed frame's trace shows the title's barrier calls | Not applicable | Barrier dump is for explicitly listed frames. Patch 0030 already measures and scopes barriers in the runtime. |
| 4243a96 | Shader: 16-bit typed buffer accesses pass bits through integer views | Useful | Descriptor/recompiler groups. Typed 16-bit bit preservation needs matching integer views, emitted access types and stride tests. |
| d75491e | GPU: a texture written in pages of its own is uploaded without hashing it | Useful | Texture group. Skipping content hashes is valid only when write tracking precisely covers the texture's private pages. |
| 2a4938e | GPU: pages with nothing mapped are zeroed in host copies without failing copies | Superseded | Local sparse read fallback zeroes holes in host copies. Patch 0044 preserves kernel recovery after a direct-read fault. |
| 9c179ea | GPU: a frame wait begun after its tick passed ends at once | Already in ours | frame::wait already consumes an elapsed tick immediately for a blocking wait. VICTORY_LATE_WAITS selects the original behavior. |
| 5c4c685 | victory: the exe's bytes are dropped once the image is mapped | Already in ours | run.rs already drops both original and prepared byte buffers after mapping. |
| 9337b35 | GPU: while lists using title memory miss, none of their writes go back | Useful | Relay/coherence group. Withholding writes after a raw guest address miss requires upstream's page-table tracking. Use the final 0a5be3c policy. |
| a3b0f36 | GPU: dropping a tracker keeps guards another mapping's tracker relies on | Useful | Relay/coherence group. Overlapping trackers must retain each other's guard ownership across unmaps. |
| b2f6c95 | GPU: upload tests count their own texture's uploads | Useful | Texture group. Isolates upload counters for the upstream lazy refresh regressions. Pair with the matching host texture implementation. |
| 38994e5 | GPU: what a frame withholds leaves the host copies at the frame's end | Useful | Relay group. Earlier invalidation point in the withheld-write series, refined by ec9e750 and 0a5be3c. |
| ed10c27 | GPU: the statistics line no longer walks the process heaps | Not applicable | The upstream process-heap walk from 2a93ff0 is absent here, so there is no walk to remove. |
| 36ca299 | GPU: a retake marks the copies without opening their guards | Useful | Relay group. Retaking dirty state without opening guards must respect alias overlap and reservation lifetime in 0045. |
| 658a0d1 | GPU: a retake marks host buffer copies alone, not textures' staging | Useful | Relay/texture groups. Narrows retake to buffer copies and depends on the same tracking distinction as 36ca299. |
| 2921092 | HLE: writes into physical pages are reported where the pages show when the write ends | Useful | Relay/service groups. Physical writes must be attributed to the mapping that exists at completion in this fork's scatter lifetime model. |
| fdf34ab | GPU: texture staging buffers come from a small shared pool, not one per texture | Superseded | Patch 0012 and the staging budget already pool texture upload buffers. |
| cc5f961 | GPU: a texture re-detiles only the subresources whose pages changed | Useful | Texture group. Page-to-subresource mapping needs local remap ranges and mip-tail handling, especially for shared pages. |
| 58b5255 | Shaders: an address half set on every taken edge into its join is known | Useful | Recompiler group. Joining partially initialized address pairs changes resource and scalar dataflow together. |
| e790217 | Shaders: an image descriptor read only for its size takes it from the host | Useful | Descriptor/recompiler groups. Querying image size through the host depends on matching emitted loads and host metadata. |
| 889b871 | Shaders: a scalar VALU result takes no EXEC guard | Useful | Wave64 group. Scalar VALU results and EXEC guards must preserve the behavior of parked lanes. |
| 4a3166e | Shaders: lanes skipping a compare's region read 0 of its lane mask | Useful | Wave64 group. Lanes bypassing compare regions need explicit mask semantics in the paired representation. |
| 47cde30 | Shaders: a test of register arrays indexed in waterfall loops | Useful | Recompiler group. Synthetic waterfall-array regression should travel with the relevant relative register lowering. |
| 52d14b1 | GPU, Shader: placed textures made when shaders read their heap slots | Useful | Texture/descriptor groups. Selective texture materialization depends on read-mark shader instrumentation and both bitmap consumers. |
| 2d6c0df | Shader: read marks only for texture views taken from the heap itself | Useful | Descriptor/recompiler groups. Restricts shader read marking to direct heap selection in the 52d14b1 series. |
| 80d8cf5 | GPU: read marks are taken from both written-page bitmaps | Useful | Relay/descriptor groups. Consumes both written-page bitmaps introduced by the shader read-mark architecture. |
| 672e716 | Shader: the read-mark test's doc comment is no list | Not applicable | Comment change for a test in the upstream read-mark series. |
| 69d1e8e | GPU: host textures fed from title memory are given up when unused, and past the card's budget | Superseded | The pin budgets and sweeps host texture copies under pressure in resource.rs, including VICTORY_TEXTURE_BUDGET_MB. Local heap placement comes from 0017. |
| 4959d34 | GPU: textures the heaps view are brought up to date only when shaders read them lately | Useful | Texture group. Recent GPU slot reads can prioritize refresh if the shader and descriptor branches carry matching read marks. |
| cea278c | GPU: textures for heap slots shaders read are made outside the heaps' lock | Useful | Descriptor/texture groups. Moving materialization outside the heap lock requires revalidation when the title rewrites a slot. |
| ed25c0e | GPU: a slot the title rewrites keeps no per-texture list of slots | Useful | Descriptor group. Removing the per-texture slot index changes rewrite invalidation and release bookkeeping. |
| c6e9500 | GPU: a texture's refresh asks its tracker's clean bit before taking its pages | Useful | Texture group. Early clean-bit exit depends on remap invalidation and the lifetime of dirty page snapshots in 0037/0045. |
| 42ea09f | GPU: what a view needs is made before its slot's lock is taken | Useful | Descriptor group. Prepare a view before locking its slot and recheck the slot identity before publication. |
| 56f75f8 | GPU: textures are brought up outside the heaps' locks, several at a time | Useful | Texture/descriptor groups. Parallel materialization includes lock splitting, staging and concurrent publication. |
| ec9e750 | GPU: what a submission that missed title memory withheld leaves the copies at once | Useful | Relay group. Narrows stale copy lifetime to the submission that withheld writes. Depends on raw-address miss handling. |
| 8ff7913 | Shader: rectangles complete opposite their right angle, wherever it is | Already in ours | The pinned geometry shader completes a rectangle opposite the right angle regardless of vertex order. CPU fallback remains a separate path. |
| 8a3d975 | GPU: textures' staging space is carved from large upload buffers | Useful | Texture group. A suballocated upload arena is a different resource lifetime from patch 0012's recycled staging resources. |
| ab2e2b2 | GPU: the textures of slots shaders read are made several at a time | Useful | Texture/descriptor groups. Parallel first use depends on read marks and out-of-lock view publication. |
| ea81289 | GPU: a submission's texture uploads become readable in one barrier batch | Useful | Texture/submit groups. Batched texture transitions must preserve ordering with early barriers and deferred uploads. |
| 0a5be3c | GPU: only what programs using title memory write is withheld when their list misses | Useful | Relay/recompiler groups. Final scope of write withholding requires shader-level knowledge of raw guest memory accesses. |
| f68442f | GPU: a texture a host copy writes is the host's from the copy's submission | Useful | Texture group. Submission-time transfer of texture ownership must agree with 0027 and pending alias writes. |
| 11e9a54 | GPU: textures fed from title memory are placed in heaps made ahead | Superseded | Patch 0017 already places textures in pooled heaps, with local residency and staging budgets. |
| 9cc37c5 | GPU: lists wait on memory values; queues hold work until its waits hold | Superseded | Patches 0021, 0034 and 0036 implement ordered queue waits, lifetime of split lists and mapped values. |
| 8f2a6f1 | GPU: a texture refresh reads title memory into its thread's own buffer | Useful | Texture group. Reusable per-thread input buffers need to fit the fork's refresh pool and memory budget. |
| 286a00e | Shaders: the DXIL writer leaves out what no edge or reader reaches | Superseded | Fork 1684989 already adds DXIL cleanup. Any further dead-edge elimination belongs with its existing bitcode optimizer. |
| 850b66f | Shaders: one handle per descriptor at a fixed address, and per join | Useful | Recompiler group. Handle sharing across joins depends on descriptor liveness and wave control flow. |
| 8681044 | Shader tests: the differential harness binds the view-stride table | Useful | Recompiler group. Device harness binding must match the view-stride layout used by the current translator. |
| a1a2077 | Shaders: vertex, geometry and mesh programs with UAVs say so in their flags | Useful | Recompiler correctness group. UAV stage flags must reflect actual shader side effects for vertex, geometry and mesh. |
| 4015a18 | Shaders: quad reads in pixel programs reach the helpers at triangle edges | Useful | Recompiler group. Quad operations and helper-lane semantics at triangle edges require pixel-stage device tests. |
| 315cea1 | GPU: barriers start from the states resources are in: prelude order, the clamp's own buffer | Useful | Submit/indirect groups. Prelude order and clamp scratch transitions overlap 0030/0046 and indirect batching. Check both initial and final states. |
| 54ab406 | Shaders: descriptors joined by nested control flow, again | Useful | Recompiler group. Final nested-descriptor join implementation after the revert. Bring its liveness prerequisites. |
| da82e44 | Shader: normalized buffer codes convert to the float nearest their quotient | Useful | Recompiler group. Final nearest-quotient conversion for normalized formats, following 645391b. |
| 67e8ae1 | Shader tests: the compute harness zeroes its UAVs | Useful | Recompiler group. Differential readbacks require initialized UAVs. Match the resource layout of the local harness. |
| 13f91ca | README: status of 2026-11-25, the prologue is reached at about 1 fps | Not applicable | Upstream status and performance refer to a different engine build. |
