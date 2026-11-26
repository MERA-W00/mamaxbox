# November correctness sweep

The checkout of XCL contains 757 non-merge commits since 2026-11-01. The inventory below follows Git's traversal order. The status table covers the method, shader and service fixes investigated for this branch. Source filenames in the second column are relative to XCL’s `crates/xcl/src`. All implementation switches in this table default to off. Set a switch to `1` in a fresh process to enable it, or `0` to use the previous path. Shader switches participate in the translation cache fingerprint. The changes use VIctory’s own list, shader IR and service code.

| XCL commits | Reference files | Finding in VIctory |
|---|---|---|
| 8e9af35, 3eab40a | hle/d3d12x/real.rs | Already correct. Older list slot 127 and newer slot 129 select the same mesh dispatch. The layout test checks both tables. |
| 1a1a07f, 3453dd9 | hle/d3d12x/list_x.rs, console_heap.rs, console_list.rs, real.rs | Fixed the missing list methods 53 and 54. `VICTORY_CONSOLE_UAV_CLEAR` makes a CPU descriptor and copies it to an execution-lived slot of the bound heap. A mismatch or a placed texture without a host UAV skips the clear. |
| 2ca749a | hle/d3d12x/present.rs | `VICTORY_PRESENT_PLANES` snapshots 72-byte plane records, crop rectangles and resource references at the call. The presentation list samples supported colour planes in order using premultiplied alpha. |
| 9251d80, 587d77b, ea4bd62 | hle/d3d12x/present.rs, real.rs | For a multiplane frame the compositor sizes the swap chain to the largest width and height across its resources. Unsupported sampling falls back to a bounded plane-zero copy. Queue order and asynchronous present already have separate support. Only the first resource and source rectangle in each plane are interpreted. |
| 78fba7b, b3a969b, 66e8e97, 11920b5, b55c478, 01d4c77, f52ec36 | hle/d3d12x/title_list.rs, alloc_pool.rs, list_x.rs | The asynchronous queue takes the host list, allocator-owned backing, uploads and draw state at detach, and leaves a closed replacement. Both reset entry points reach the title wrapper. Guest allocators have no host recording to erase. The existing queued-wait test also resets and releases a list while the queue is held. |
| cad94a2 | hle/d3d12x/list_x.rs, real.rs | Empty heap binding already clears both heap slots in draw state. |
| 0306d10 | gpu/recomp/emit.rs, tests.rs | The general first-lane lowering already uses WaveReadLaneFirst or ballots the per-lane EXEC predicate before reading the selected live lane. Proven uniform values and recognized descriptor waterfalls have their own paths. |
| 8fcc111, 12f25f1, 39ef551 | gpu/recomp/emit.rs, mod.rs, tests.rs | The DXIL emitter lowers supported mask reductions to wave votes or ballots. These exclude pixel helper invocations. XCL’s HLSL mask reconstruction is absent here. Split-wave compute remains a separate workstream. |
| ea088a5, 9fb040d | gpu/dxil/lower_image.rs, gpu/recomp/emit.rs, tests.rs | `VICTORY_RESINFO_MIP_BOUNDS` checks the requested unsigned mip against the view’s level count. All sizes become zero past the end. The count survives. |
| 71d94e7, f884dc5, cafda82, 0056934 | gpu/dxil/lower_image.rs, gpu/recomp/emit.rs | `VICTORY_RESINFO_SHAPE` corrects depth for non-array 1D and 2D images, reports 1D array layers in y and z, and reports six faces for VIctory’s single-cube view. Arrays of cubes also need the corresponding binding and sampling support. |
| a334936 | gpu/dxil/lower_image.rs, gpu/recomp/emit.rs | Partial. `VICTORY_RESINFO_MSAA` supports explicit 2D multisample and multisample-array query instructions. A regular 1D/2D query pointed at a multisample descriptor still needs runtime view-type information. VIctory stores an engine record in the title slot. Its fourth word cannot be used as a hardware T# type. |
| 4070ae1 | gpu/dxil/lower_image.rs | `VICTORY_RESINFO_D16` packs the requested integer components, low half first, into the number of VGPRs reserved by the decoder. |
| 3eab40a | gpu/recomp/emit.rs | `VICTORY_MESH_PRIM_9BIT` ignores the reserved bit in each packed primitive index before testing the declared vertex count. |
| 83ad6a5 | gpu/recomp/mod.rs, gpu/recomp/lower_dxil.rs, gpu/dxil/kernel.rs | `VICTORY_COMPUTE_WINDOW_COHERENCE` marks the raw page-window UAV globally coherent when compute instructions request cache coherence, perform memory atomics or invalidate caches. The fault-report UAV and directory keep their own properties. |
| 9cdae10, d4c6d34 | hle/kernelx.rs, hle/d3d12x/pso_async.rs, device_x.rs, real.rs | `VICTORY_TITLE_SLEEP_HOLD` discounts overlapping synchronous GPU submit, placement and pipeline lookup time from finite title sleeps of at least 100 ms after 250 ms of such work in a frame. The clock counts simultaneous calls once. This changes watchdog timing and should be exercised independently. |
| ed56bca | hle/netoff.rs | Extended and basic resolver hooks already shared one policy for localhost, null and literals. `VICTORY_NETWORK_LOCAL_NAME` also recognizes the host’s exact name and asks Winsock for loopback, including through the extended asynchronous entries. |
| 28ee5d7, 9a85c6e, ed37916 | hle/xgameruntime/networking.rs | The offline provider cannot fetch security information and has no certificate-chain parser, pin validation or serialized host security structure. The three errors in the online implementation do not apply. |
| bb27ed6, d0b6079, 3724b07, c8e3ca0 | hle/dstorage.rs, kernelx.rs | The worker already reads a heap page list at execution. `VICTORY_DSTORAGE_PAGE_LIST` preserves a stack list at enqueue and makes a checked, owned copy of a deferred list before writing. Invalidated lists fail the request. |
| c8e3ca0 | hle/dstorage.rs | `VICTORY_DSTORAGE_FILE_SOURCE` resolves an opaque file object against the factory’s live files at enqueue. The queued read owns the open file and its name through execution. A foreign source fails with the file-handle HRESULT and can still be cancelled before it runs. |
| 66660bd, 6abe17d | hle/dstorage.rs | `VICTORY_DSTORAGE_CANCEL` implements masked tag cancellation of enqueued and submitted reads that have yet to start. Status entries account only for commands that run. |
| 2e17d3d, a4b2e92 | hle/dstorage.rs | `VICTORY_DSTORAGE_ERROR_RECORD` retains the count and first failure, including offset, source size and tag. The optional request-name field is empty. Event retrieval duplicates the queue’s handle. Reading the record resets the event and count together. |
| 5bd7cd8, 1495e6d, 8cad517, f13bc2e | hle/dstorage.rs | Status entries already start at S_OK, a worker pool already runs independent reads, BCPack decoding is implemented, and refused factory IDs are already logged. |
| 1028ba5 | hle/xgameruntime/gamesave.rs | Blob enumeration already collects names and sizes under the save lock and releases the lock before calling the title. |

## Verification

The following suites passed with toolchain 1.98.1, first with the switches at their defaults and again with the new paths enabled. Tests added for this sweep use synthetic inputs.

| Suite | Passed per configuration | Ignored |
|---|---:|---:|
| GPU library, one test thread | 207 | 12 |
| Shader | 222 | 3 |
| HLE | 107 unit and 1 integration | 0 |

The GPU readbacks checked integer and float UAV clears restricted to a rectangle, two premultiplied colour planes and a fallback copy into a wider target. A synthetic shader readback checked the packed size and an out-of-range mip for each lane. The explicit multisample and multisample-array programs went through translation and the available DXIL validator. Other shader tests cover the visibility decision and the reserved mesh-index bit. Storage tests cover replacement page lists, file release before execution, foreign file objects, cancellation ordering, first failure and event reset. Synthetic clock tests cover overlapping holds and finite sleep accounting.

The locked x86-64 Windows diagnostic build of the `victory` binary succeeded with diagnostics enabled.

## Review and measurement

The 300-second baseline log contained zero DirectStorage request failures and zero host command-list reset errors. Compare `console UAV clears` (recorded and skipped) and `multiplane present` (composed and fallback) with the existing presentation and translation counters. The two counters log at powers of two. The compositor counts frames that reached recording. An unresolved resource in the initial snapshot takes the existing plane-zero path before that counter. The storage diagnostics report worker requests and staging waits. The existing `VICTORY_WALLS_TRACE=1` logs each failed worker request with its HRESULT, offset and sizes, including file and page-list validation failures. At debug level, `title sleep extended` reports the requested and held milliseconds.

The final baseline submit-thread line reported 85.8 ms of work and other time and 15.2 ms awaiting the relay. Compare those fields, frame interval, and shader translation failures in an A/B. All 14 switches stay off pending title and driver validation. The GPU readbacks cover a normalized colour target, and the synthetic sleep test cannot establish how a title’s watchdog reacts. No in-game timing was taken for this sweep.

## Commit inventory

The following is the complete short-ID output of `git -C ../../local/ref/xcl log --since=2026-11-01 --no-merges --format=%h`, wrapped across columns.

```text
83ad6a5 0829441 e9ddf17 625a0f3 12f25f1 8fcc111 0306d10 8e9af35 3ea5edc ed56bca d2515bc f8ce21d
1a1a07f f6fa8a9 ad03e65 697e8fd 6c34370 e2db0c0 95f8d26 87edf7e 9a257e1 ea088a5 e964ad2 cb6b7b6
040e46d 621bbdd 0d13b07 283068c 482cb12 e64bd17 547886c b4d7482 19bef42 2fe270d 9fb040d df45043
1028ba5 78fba7b ec356bc 77bb02d f0e1b2f 45b6e18 04e9cee a334936 6ea813a 637b82a 6e1e2be cec6085
da247f1 8bd6aa7 8dcc56d bb35b45 bae19e9 4b9eb9b 6987e49 c390263 2f27167 330a365 3e1eab2 2cc20ac
9c8c048 254a058 743a612 bb27ed6 4510bf8 71d94e7 dea3552 6dbae61 f884dc5 e591a8c 7f0c1a3 27b54e1
18423c3 49a2546 6c9f947 ce22d27 5a515f0 8c1faf1 4b0af47 ff184be 3eab40a a7b478b 587d77b 71639e0
ae00ec7 aaf2c78 2c1102d 4a6b414 daf52ab 5f4d32f 8921c36 2bea7ac c7f6933 b937c33 3705368 1c971de
d8391cd 1512961 7881dbc 990b9d4 3229445 06308bc 0f2e2d2 24162c7 dc0d25f fb9d059 2091b24 c4396e2
646b439 d4c6d34 5195e27 7a1f5a2 878742d 513a18e 354ce0c 892177b 25298fb 415fc5e 5b31ca7 714d8c7
1f8d4be f7bd213 ffc9081 50228bb 4070ae1 79fde18 b5767c3 1831ec4 3f50b79 9492f43 74fe4c2 438d420
9eaf878 47e9356 3d32dbb e22a931 425b869 e1217e7 fe78c81 2448457 04f34ce ecb7547 15fff3b 62fcf07
c6067a6 6b9b14f 1d027e5 cbff0b8 28946bb 3bc7143 bfb8133 9dcf2f6 66879ef 9e27370 4e057cb f251762
29eccc0 14e367e e40ac38 fdb4c10 dd566c0 a23b6ff 3827fb5 27751a0 fe8dc84 188ee17 82c875d 3aea7a9
b1bfd1e 5d2d65c a24899e 27d28ce d2cc6ff 77a5eee 372be9d bc0d77c 5963c26 ddfbf39 878afda 77db49c
ad39e4d 9231d48 acdf922 f9be7cc 8593700 6bdef80 9816a85 a5311f7 fbb3a7b 45025eb f5d3e05 ac8f8b6
f90033c 13ac196 ac34cb6 511eadd f33f640 430f0a3 b3c0b37 9b8db83 1bab757 9486494 971c0e5 fa56795
7a84e7a 2a2d54b 0b64aa0 69a892c b5d6322 4143a09 051d2c8 87d421b 1495e6d ee85bc2 210f773 f458b55
65154b3 2dcc926 b07fbab 81d2ad5 76c527b ac4799f 6976bc4 d24db9f def2134 ee7f2f3 4e31065 424d315
223311f 3a06645 724b9e2 9f65f95 dc2d8f1 7d24e97 9251d80 469d8b4 f372cfb 34acfbf deb4b92 927adcc
cad94a2 fe7d70b 9cdae10 185810f b6239db ac9328f 55e9c3b 3c01233 02ec584 942b6c7 86ab4cd b45a6c9
97d5259 065aef3 746f7dd 6b1170b 7c672cd 9230fbc 2815b2a cbde110 a8569bf 40c81c1 8c3cfd8 39ef551
60e7603 5383506 930e8b4 b8b1f49 9a272d4 25c6f7e d6ff44c 35665aa cafda82 615cb20 365d7c8 560216b
e642921 5bd7cd8 2e17d3d 392dc0c 21bc548 38283a8 1d83fd1 f4bbf26 66660bd c237ebb de85f97 7d834d4
7bea019 3b6e9ea 8f66b3b cf244b0 21de1a8 47efa2e d78597d 24d8b92 4f4db64 d9fcdce 9a19d7e 01d4c77
b55c478 f52ec36 10915eb 8add9a3 8cad517 c1a9b52 ef48a18 02bfd1d 87fae1f 6522c24 9e2c7a5 9aec53d
e1c4020 38992ed 38ea451 b3a969b 9beaa73 077ce40 71bda93 8cada26 5b693e6 5c3ff31 11920b5 1ab9ba9
5fab112 03912e8 dbe9324 3544038 e7da8a1 f1e2113 2239a06 e50836b 285560f 8a5643f 0c02b87 f9e5ee9
066ae5b d208e14 3f54f4d 5a60d0d dfc5578 242a6ea 07c535d 4e0acbd 1e181b7 2cc79fb 2a2cc6d 9683e7a
7296a8b 91f9a5e 9182f27 1347bea 0fb9f4a 9461cf7 2ca749a b884f3c d3bd51d c3d350a b9f3d0f 3984bf1
3ae0841 9fb0363 b74867b 86a5d0b 2d9a9fe 38e58e1 70f9fce 0db1c5a d96812a e6d5d33 712bbbe 6aba068
b86fba4 3724b07 7f94c6f 69f0635 27c3571 1f282c7 3462d40 7695539 cf6157a 2997c9a 2cefc88 7d60a48
c55783e f7fd232 a94e151 a6f5f7a ef5312f fa9ad89 97dc49c 3516080 bb436a0 1130bc5 2b4bb43 4403887
29fb6b7 db1ddb2 01a7446 d6cbfcd 603cdd3 c8e3ca0 ee0adee ad79932 a99f64c 7bdc97f 2e67909 9bd7635
e93dc9c 0e946cf 9507d05 d0b6079 9e9551b ebbf377 d54597a f03fe38 2c5fe3f 2f5979e 9ab7d76 63b9930
8adb9dd 1775ebb 7a033c8 77905f6 c858190 608a79b 5ba2b6a 60565c4 6b9f2ba 8a98da6 bc59624 d4f17ca
594f63a 20271d4 327c24c b25582a af20b4b 7b029e2 09863ba 247d909 0cd0a53 a0dce86 b856022 5ceb8fc
033a6d1 7ac4048 82cba12 938fdd2 c217b1f 4bf5466 fa2d1d3 5a537b3 1b0c610 8fde77a f17a5ee eaa36b6
68a6640 31635bd e8564f1 488e5fe 38b05b7 bb66204 4c3c156 9392eae 0b28a39 3d99e80 b8a7794 62e0e25
af46b71 c69c4d1 782e45b e80b5b3 d66f4c7 f7c7a48 a3dedf1 5911fb0 a6e52be 229708a 10b50ea fc4b2d4
f13bc2e deccf2b 956a476 c00a305 323767f db5dc26 66e8e97 b210576 9add721 6cb71db d21e372 1ffa570
7495271 87fe289 8e2aec9 b5380ad 20a35f0 0b89bdf 1058c0b 0511fed c8d9549 484dd57 55156f0 fc60bf5
af0311d 95bda11 70f8dc4 c4df2fe fdeacf0 23c53fc 1a51a82 3304bb1 a787c52 70d7e0a a01d61c 34efd81
0056934 c806a0b ad48c26 f7a9f5c 8ba374c 517d983 56fa1dc 07f12e8 58309b9 ef4aae1 ea4bd62 f90e644
668f550 02d1302 41cc3d5 b3dd82d aaaa7ad c85cd6f c9c8677 eac85f9 0a3b802 2d94699 d7eae06 d903a80
c204343 9cd48da 3885b86 60599a9 d1e8d9e 3121db5 b4019c4 ecbad66 697dbb1 1f82be3 9284dcd 22d21f0
079b541 d70f8e9 71c4b9b c2887bb b4edefb 6a41c55 55df81b ba905cf 46a1c3f 593a237 848e740 ff5c940
e38cb65 8064fe3 93072dc 7ac0828 73fb221 15243ad 30d1e67 8ad0b44 98cb80a 385dc5f 8138894 a4b2e92
527fe98 1ad0d4a 31a4001 3b74cda 4ee1b6e ed37916 9a85c6e 6ececa1 a8918b7 21e42cf b8fca79 ba81c3d
66e23c5 21bbb85 ab71385 8a52568 3ecf4cf 22ebfae 319bcd6 c8d4a2f 654b87b 3283d11 80489d0 817328d
33efe92 f6a249f 935155c 2ca2b73 f2d5b6f 5c6bbc5 cfed905 6a270f5 48aabc1 a081a65 a3feb68 ff9430e
61c6be8 521ec08 9a15c53 4e5c45a 7b42c08 28ee5d7 912bb5d aac59d6 4de9016 798ff0d afaf413 a312759
5dd2d56 7396683 bb9f0b5 acbbc55 8ebba61 5c15581 3031945 2bc2e37 2dd04da 4a8344c 004b3db 126268c
145bc56 6cee70f bb58372 550d979 e80ce8d 1903ff6 9e759de 8eb3cf4 546f056 6c5af1b 6038bb1 3a8bc96
507c441 74eaad0 0fac0d3 c2e6ce9 c13c774 af6aaa1 0fb82c6 660f6dc 3dfbcfb e7ca2cd f6ad90c e89985f
ba422b7 58c1f42 97e103d f12d724 6a27beb 6abe17d e555b87 6da1947 18cb400 92beeab fd74662 bdae5a4
3c546ef a379e68 68c2c20 c2e1f8d 70835a2 c768791 f81441a b8151eb 972c589 ea98a6b fae7655 3f68644
2ff23c3 eddf5a7 50688d5 d5703bf d4dc2f5 861869f 5a5c32b 83f476b a3ec898 b2cc8ed b81284c 28c95b4
122bff2 55ebf2b 660a62a 88b68b4 f410fa1 813172a d0d3062 27bc5c2 c9565af b8f4b0a fb3e8e8 23c7099
5aae781 7c98008 59eaf46 eec9fb4 b1477f8 e819d36 c3049b0 4cf10c7 06a4a49 3365f6e 2bd8fcc c3b10f1
d575a8c a6ccf9c 1c8fae9 61492f6 6e92fa2 576b4db 3235ebe 23e241f f3b15b9 0b17b2e 98d32ec 885e6a2
c6996f7 092a330 c2a9ce6 77aefa5 26cf5ec dc902f1 71d2f82 c2123ec 74cd9db b64dbd3 a89f714 2b7e79b
3453dd9 bb363d1 9f217bc a02eb66 700c83e 8c6f1c3 cc29f47 575c768 b56a1b2 e714e2a 57b529c a21d3df
8fa7fa0
```
