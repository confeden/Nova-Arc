# GPU-Accelerated Lossless Compression — State of the Art (2024–2026)

Research for Nova Arc (`.narc` format: append-only, CDC chunks 256 KiB–4 MiB, blake3 dedup, zstd baseline).
Target hardware context: mid-range Windows PCs, NVIDIA RTX 5060 Ti 8 GB (consumer Blackwell), 32 GB RAM.
Researched live 2026-08-16. All liveness/license claims verified against GitHub API / vendor docs on that date.

---

## 1. Executive summary

- **GPU lossless compression is real and fast, but only for *fast* codecs and *batched* workloads.** nvCOMP-class libraries reach 100–600 GB/s decompression and 10–100 GB/s compression on datacenter GPUs — but on Silesia-like general data with a mid-range GPU, expect single-digit GB/s, and everything is capped by PCIe (~12–25 GB/s practical) because an archiver's data starts and ends in host RAM.
- **The only production-grade GPU compression library is NVIDIA nvCOMP — and it is closed-source (proprietary EULA) since v2.3 (2022).** Current version 5.3 supports LZ4, Snappy, Deflate, GZIP, zstd, GDeflate, ANS, Bitcomp, Cascaded + CRC32. It may be *used* and its redistributable binaries shipped inside an application, but it cannot be vendored into an open-source repo or statically redistributed standalone.
- **The cross-vendor story improved dramatically in 2025–2026**: DirectStorage 1.4 (GDC 2026) added **Zstandard GPU decompression with an open-source (MIT) compute shader**, tuned for chunks ≤ 256 KB — almost exactly narc's chunk regime. Vulkan got `VK_EXT_memory_decompression` (GDeflate, multi-vendor). These are *decompression-only* paths.
- **Consumer Blackwell (RTX 5060 Ti) has NO hardware Decompression Engine** — that fixed-function block (Snappy/LZ4/Deflate at ~400–600 GB/s) is datacenter-only (B200/B300/GB200/GB300). On RTX cards nvCOMP runs SM (CUDA-core) kernels, which still work well.
- **Where GPU genuinely wins for an archiver**: batch decompression of many independent chunks; batch entropy coding (ANS at hundreds of GB/s); fast-codec compression when CPU must stay free (which is a narc hard requirement — GPU offload *is* a responsiveness feature); checksums of GPU-resident data. **Where it loses**: ratio-oriented codecs (large-window match finding is branch/latency-bound — no viable GPU LZMA/zstd-19 exists), blake3 hashing (CPU SIMD already saturates; PCIe transfer erases any win), small archives (launch + transfer overhead), and VRAM contention on 8 GB cards.
- **Recommendation in one line**: keep `.narc` codecs bitstream-standard (zstd/LZ4), put GPU behind an optional `Codec`-provider trait + cargo feature, implement NVIDIA first via nvCOMP FFI with runtime dynamic loading (cudarc-style dlopen, never a hard link), and treat a wgpu/WGSL cross-vendor decompressor as a later experiment seeded from Microsoft's MIT zstd shader.

---

## 2. NVIDIA nvCOMP — the only production-grade option

### 2.1 Status and requirements (verified 2026-08)

| Item | State |
|---|---|
| Current version | **5.3.0** (docs.nvidia.com/cuda/nvcomp); 4.2 added Blackwell, 5.x series is current |
| Source | **Closed since v2.3 (2022)**. GitHub `NVIDIA/nvcomp` is archived, docs/examples only; examples now live in `NVIDIA/CUDALibrarySamples` |
| License | Proprietary NVIDIA SDK EULA (see §2.4) |
| OS | Windows ≥ 10, Linux x86_64/arm64 |
| GPU | **Volta (sm70) or newer** — Pascal support dropped in 5.x. RTX 5060 Ti (Blackwell) is supported |
| CUDA | **CUDA Toolkit ≥ 12.0** (CUDA 11 builds dropped at 5.1); driver ≥ 527.41 on Windows |
| Distribution | pip (`nvidia-nvcomp-cu12/-cu13`, `nvidia-libnvcomp-cu12/-cu13`), conda-forge, tarball from developer.nvidia.com, Linux distro packages |
| APIs | C, C++, Python; low-level **batched API** (many independent chunks — matches narc's chunk model) and high-level API (adds nvCOMP's own framing header) |

Sources: [nvCOMP docs](https://docs.nvidia.com/cuda/nvcomp/index.html), [installation](https://docs.nvidia.com/cuda/nvcomp/installation.html), [release notes](https://docs.nvidia.com/cuda/nvcomp/release_notes.html), [developer page](https://developer.nvidia.com/nvcomp), [archived GitHub repo](https://github.com/NVIDIA/nvcomp).

### 2.2 Codecs

| Codec | Compress on GPU | Decompress on GPU | Bitstream-standard? | Notes |
|---|---|---|---|---|
| LZ4 | yes | yes | yes (raw blocks) | interop with CPU lz4 both directions (CPU-compress examples exist) |
| Snappy | yes | yes | yes | newly re-optimized in 5.0 |
| Deflate | yes | yes | yes | decompression up to 1.5× faster in recent releases |
| GZIP | yes (added 5.x) | yes | yes | gzip framing for existing workflows |
| **zstd** | yes | yes | **yes (standard frames)** | narc's baseline codec; ~16 GB/s compress reported on Blackwell; decomp optimized 2.2× on H100 |
| GDeflate | yes | yes | open spec (64 KB tiles) | same format as DirectStorage/Vulkan GDeflate |
| ANS (gANS) | yes | yes | **proprietary** | entropy-only, extremely fast; 5.3 added fp8 mode, up to 2× throughput gain |
| Bitcomp | yes | yes | **proprietary** | numeric/HPC data |
| Cascaded | yes | yes | **proprietary** | RLE+delta+bitpack; brilliant on tabular/columnar, poor on general bytes |
| CRC32 | — | — | — | GPU-optimized checksum |

Key interop fact for narc: the standard-format codecs used through the **low-level batched API** produce/consume standard bitstreams per chunk, so **a `.narc` written with GPU zstd remains extractable by pure-CPU zstd on any machine** (and vice versa: CPU-written chunks can be batch-decompressed on GPU). This is the property that lets GPU stay an optional accelerator rather than a format dependency. (Precedent: [zarr's GPU zstd codec](https://github.com/zarr-developers/zarr-python/pull/2863) decodes CPU-written zstd with nvCOMP.)

### 2.3 Performance — verified numbers with context

Vendor headline numbers are on datacenter GPUs and/or highly compressible columnar data. Reality-check rows included.

| Setup | Codec | Compress | Decompress | Ratio | Source |
|---|---|---|---|---|---|
| A100, mortgage column (highly compressible) | LZ4 batched | 95.9 GB/s | 320.7 GB/s | 38.9 | nvCOMP benchmark docs |
| RTX 3090, mortgage column | Cascaded | 225.6 GB/s | 375.0 GB/s | 39.7 | nvCOMP benchmark docs |
| RTX 3090, mortgage column | LZ4 | 36.6 GB/s | 118.5 GB/s | 21.2 | nvCOMP benchmark docs |
| **GTX 1650 mobile, Silesia.tar (general data)** | LZ4 | **0.57 GB/s** | **5.28 GB/s** | ~1.97 | encode.su community test |
| Blackwell (B200), Silesia | Snappy/LZ4/Deflate via **HW DE** | n/a (decomp-only) | up to ~400–600 GB/s | ~2× | NVIDIA blog |
| Blackwell, checkpoint data | zstd | ~16 GB/s | — | — | NVIDIA/community checkpoint blog |
| H100 | nvCOMP ANS | — | ~480 GB/s | entropy-only | 2025–26 academic measurement |
| A100 | dietgpu ANS | 250–410 GB/s | 250–410 GB/s | entropy-only | dietgpu README |
| H100 | dietgpu ANS | 364.9 GB/s | 592.5 GB/s | entropy-only | 2026 paper measurement |

Interpretation for narc on an RTX 5060 Ti:
- General-purpose data ≈ Silesia, not mortgage columns. Scale the 3090 numbers down (~70% of the SMs / bandwidth) and expect **order 5–20 GB/s LZ4-class decompression, 1–5 GB/s LZ4/zstd-class compression** on-device.
- **PCIe is the wall**: the card is PCIe 5.0 x8 (~32 GB/s theoretical, ~25 GB/s practical; on a PCIe 4.0 board ~13 GB/s). Archive data must go host→GPU→host, so effective throughput ≤ half of that — still comfortably above NVMe read speed (3–7 GB/s), so the pipeline bottleneck becomes disk, which is the correct place for it.
- Chunk-size sensitivity is high (thesis + Voltron Data tuning experiments): smaller chunks → more parallelism → higher throughput but worse ratio. narc's 256 KiB–4 MiB CDC chunks sit in the sweet band; nvCOMP internally likes 64–512 KiB (DE benchmarks use exactly 64 KiB/512 KiB).

Sources: [nvCOMP benchmarks doc](https://github.com/NVIDIA/nvcomp/blob/main/doc/Benchmarks.md), [encode.su nvCOMP thread](https://encode.su/threads/3626-nvCOMP-nVidia-compression-library), [Blackwell DE blog](https://developer.nvidia.com/blog/speeding-up-data-decompression-with-nvcomp-and-the-nvidia-blackwell-decompression-engine/), [DE FAQ](https://docs.nvidia.com/cuda/nvcomp/decompression_engine_faq.html), [Voltron Data chunking experiment](https://voltrondata.com/blog/gpus-analytics-experiment-with-tuning-chunking-compression-decompression).

### 2.4 License deep-dive (the part that matters for an open-source archiver)

nvCOMP is governed by the **NVIDIA Software License Agreement / nvCOMP EULA** ([text](https://github.com/NVIDIA/nvcomp/blob/main/LICENSE), conda tags it `LicenseRef-nvCOMP-Software-License-Agreement AND LicenseRef-NVIDIA-End-User-License-Agreement`):

- **Granted**: non-exclusive, non-transferable use; **distribution of the designated redistributable binaries as part of your application**, provided the application has "material additional functionality" beyond the SDK and only your application accesses the SDK portions.
- **Prohibited**: standalone redistribution, sublicensing, reverse engineering, modification/derivative works of the library; your distribution terms must not conflict with NVIDIA's (this is friction with copyleft licenses; MIT/Apache app licenses are fine in practice since the EULA only covers the NVIDIA binaries themselves).
- **Practical consequences for Nova Arc**:
  1. Do **not** vendor nvCOMP binaries in the git repo or in default release archives of an MIT/Apache project.
  2. Safe patterns: (a) load `nvcomp.dll`/`libnvcomp.so` **dynamically at runtime** if the user has it (pip/tarball install, or shipped with RAPIDS etc.); (b) an optional separate "GPU pack" download that the user fetches from NVIDIA or that the installer pulls via pip — the EULA permits app-bundled redistribution, but keeping it out of the OSS tree avoids all ambiguity.
  3. FFI bindings themselves (a `-sys` crate with function signatures) are your own code — no license issue.

---

## 3. DirectStorage GDeflate + Zstd on Windows (cross-vendor via DirectX 12)

- **GDeflate**: Deflate-like format restructured for 32-lane parallel decode (64 KB tiles, bit-swizzled substreams). Open specification; reference CPU compressor + HLSL GPU decompressor in [microsoft/DirectStorage](https://github.com/microsoft/DirectStorage) (repo MIT, active — last push 2026-08-14). Cross-vendor by design: any DX12 SM6.0 GPU via DirectCompute fallback; vendors ship optimized "metacommands" (NVIDIA RTX IO since 526.47, Intel since 101.3793; AMD later). Measured: Arc A770 21.7 GB/s, RTX 4080 15.3 GB/s, RX 7900 XT 14.6 GB/s decompression.
- **DirectStorage 1.3 (mid-2025)**: scheduling API (`EnqueueRequests`), fixes. **1.4 (GDC 2026)**: adds **Zstandard** on both CPU and GPU paths, **open-sources the Zstd GPU decompression compute shader** (initially tuned for content chunked ≤ 256 KB), IHV driver optimizations promised; BC7 post-processing later. ([DevBlogs 1.4 announcement](https://devblogs.microsoft.com/directx/directstorage-1-4-release-adds-support-for-zstandard/), [Tom's Hardware](https://www.tomshardware.com/video-games/pc-gaming/microsoft-debuts-directstorage-1-4-at-gdc-2026-with-zstandard-compression-and-gacl-update-promises-developers-improved-compression-ratios-faster-loading-and-more))
- **Adoption reality**: weak in games (a handful of titles; GPU decompression steals frame-time). Irrelevant for narc — an archiver has no frame budget to protect, the machine is otherwise idle-ish, and freeing the CPU is a stated goal.
- **Limits for narc**: DirectStorage is **decompression-only** on GPU, D3D12/Windows-only, and its API shape is asset-streaming (file → GPU resource), not host-buffer → host-buffer. The valuable artifacts for narc are the **formats and the open shaders** (GDeflate HLSL, Zstd compute shader), not the runtime.
- **Vulkan path**: `VK_NV_memory_decompression` (RTX IO, Windows+Linux) was contributed to Khronos and became **`VK_EXT_memory_decompression`** — multi-vendor GDeflate 1.0 memory-to-memory decompression (64 KB regions), including indirect/GPU-driven variants. ([EXT proposal](https://docs.vulkan.org/features/latest/features/proposals/VK_EXT_memory_decompression.html), [NV sample](https://github.com/nvpro-samples/vk_memory_decompression)). Driver support beyond NVIDIA still needs verifying per-vendor at runtime.

## 4. AMD Brotli-G

[GPUOpen-LibrariesAndSDKs/brotli_g_sdk](https://github.com/GPUOpen-LibrariesAndSDKs/brotli_g_sdk) — MIT, Brotli variant restructured for data-parallel decode. **CPU compressor + CPU/GPU (HLSL, DX12 SM6.0) decompressor**; v1.1 added BCn texture pre-conditioning; integrated in Compressonator 4.5.
**Liveness: dormant — last push 2024-04-18** (verified via GitHub API), no releases since v1.1 era, no Vulkan/Metal port, no sign of 2025–2026 activity. Cross-vendor in theory (HLSL), but effectively a finished AMD tech demo. Not a build-on foundation for narc; its bitstream spec is still a useful reference for "restructure existing codec for GPU" techniques.

## 5. Cross-vendor compute path: wgpu / WGSL

- **No mature WGSL/WebGPU lossless codec exists today** (verified by search; nothing on crates.io/GitHub beyond toy demos). Anyone wanting cross-vendor GPU decompression in Rust must port GDeflate HLSL or Microsoft's new Zstd shader to WGSL themselves.
- **wgpu maturity (v26–28, 2025–2026)**: active and production-quality for compute; **subgroups** (critical for decompression kernels — GDeflate's design is literally "32 lanes cooperating") are supported as a native-only feature with minor spec gaps (`subgroupElect`, quad ops); Chrome shipped WebGPU subgroups in 134, so the feature is stabilizing. Timestamp queries, push constants, 64-bit atomics available on native. ([wgpu features](https://docs.rs/wgpu/latest/wgpu/struct.Features.html), [subgroup tracking issue](https://github.com/gfx-rs/wgpu/issues/5555))
- Honest cost estimate: a WGSL GDeflate **decompressor** port is a few weeks of expert work (the HLSL is ~simple, the format was designed for this). A cross-vendor **compressor** is a research project — nobody has shipped one.

## 6. Academic and open-source landscape (liveness verified)

| Project | What | Perf claim | License | Liveness (verified) | Verdict for narc |
|---|---|---|---|---|---|
| [dietgpu](https://github.com/facebookresearch/dietgpu) (Meta, Jeff Johnson) | Generalized rANS entropy codec + float codec, CUDA | 250–410 GB/s A100; 364.9/592.5 GB/s H100 enc/dec (2026 paper) | **MIT** | **ARCHIVED** (archived; last push 2026-03-18) | Best open GPU-ANS reference; usable only as a vendored fork |
| [hipANS](https://github.com/PAA-NCIC/hipANS) | dietgpu port to AMD ROCm/HIP | similar class | MIT-derived | active-ish research | proves ANS portability off-NVIDIA |
| [multians](https://github.com/weissenberger/multians) (ICPP'19) | Massively parallel tANS decode | up to 39× vs zstd entropy stage (V100) | **LGPL-3.0** | dead — last push **2019-07** | idea source only; license + decoder-only + dead |
| MANS (SC'25) | portable multi-byte ANS enc CPU+GPU | paper-stage | — | new research | watch |
| Recoil (ICPP'23) | parallel rANS with random-access metadata | paper-stage | — | research | relevant to seekable archives |
| CULZSS (2011) / GLZSS (2014) | LZSS match-finding on GPU | 18–34× vs serial CPU LZSS | academic | dead | historical only |
| [GPULZ](https://arxiv.org/abs/2304.07342) (ICS'23) | LZSS for multi-byte data on modern GPUs | up to 272× vs prior GPU LZSS (A4000) | academic | research | shows GPU LZ *compression* is possible but ratio-weak |
| [CODAG](https://arxiv.org/abs/2307.03760) (2023, NVIDIA+UIUC) | framework; shows GPU decompression is **latency/sync-bound, not memory-bound** | 13.5×/5.7×/1.18× vs RAPIDS (RLE/Deflate) | academic | research | key insight: warp-cooperative design matters more than bandwidth |
| [libcubwt](https://github.com/IlyaGrebnov/libcubwt) | GPU suffix array + BWT construction | GPU BWT for bsc `-G` mode | **Apache-2.0** | **active** (last push 2025-08-14) | only if narc ever grows a BWT codec; author (Grebnov) also maintains libsais/bsc |
| [GST](https://github.com/GammaUNC/GST) (2016) | GPU-decodable supercompressed textures (entropy-coded BCn) | texture-specific | no license file | dead — last push **2017-09** | superseded by Basis Universal / GDeflate pipelines; skip |
| [Gstd](https://github.com/elasota/gstd) | zstd→GPU-friendly transcode experiment | only ~3% smaller than GDeflate | — | **ARCHIVED, decoder non-functional** | author's negative result: zstd-transcode gains over GDeflate are marginal |
| GPU FSST (VLDB ADMS'25) | string compression on RTX 4090 | 2.8× vs nvCOMP Snappy, 7.9× vs zstd | academic | new | columnar/string niche |
| [libgiddy](https://github.com/eyalroz/libgiddy), [GPU-lossless survey](https://github.com/dingwentao/GPU-lossless-compression) | older GPU decompression kits / survey | — | mixed | stale | background reading |

### GPU hashing for dedup (blake3): **negative result**
Verified across CUDA ([Blaze-3](https://github.com/Blaze-3/BLAKE3-gpu)), SYCL, and experimental Vulkan implementations: GPU blake3 **does not decisively beat multi-threaded SIMD CPU blake3**; one CUDA fork measured *worse than a single CPU core*, the Vulkan experiment couldn't beat CPU, SYCL only edged a 64-core CPU at 64 MB inputs. Reasons: blake3's Merkle tree already parallelizes perfectly on AVX2/AVX-512; PCIe transfer costs as much as hashing; dedup pipelines are I/O-bound anyway. **narc should keep blake3 on CPU permanently.** (Only exception worth revisiting: hashing data that is *already* in VRAM as part of a GPU compression pass — but then CRC32-on-GPU via nvCOMP is the cheaper integrity primitive, while blake3 chunk IDs are computed CPU-side during CDC anyway.)

---

## 7. Rust integration

| Crate | Role | State (verified 2026-08) | Fit |
|---|---|---|---|
| [cudarc](https://github.com/coreylowman/cudarc) | safe CUDA driver/runtime/NVRTC bindings | **0.19.9, released 2026-08-11, MIT/Apache-2.0, 7.0 M downloads, very active** | **primary choice**. Key feature: **dynamic loading by default** — no CUDA needed at build time or on user machines; probe at runtime, fall back cleanly. Supports CUDA 11.4–13.x via feature flags |
| [cust](https://github.com/rust-gpu/rust-cuda) (Rust-CUDA) | write GPU kernels in Rust (NVVM backend) | rebooted 2025-01 after 3 years dormant; self-described early, **pinned to a specific nightly** | not production-ready; avoid for narc core |
| [wgpu](https://github.com/gfx-rs/wgpu) | cross-vendor compute (Vulkan/DX12/Metal) | mature, active (v26–28) | the only realistic cross-vendor path; requires writing WGSL kernels ourselves |
| nvCOMP bindings | FFI to closed lib | **no mature crate.** `baracuda-nvcomp`/`-sys` (0.0.1-alpha, 2026-08-15) is scaffolding with a dynamic loader; `s4-codec` (1.5.2, 2026-07) demonstrates exactly the pluggable "nvCOMP zstd/Bitcomp GPU + CPU zstd fallback" pattern but is niche (<1 k downloads) | **write our own `narc-nvcomp-sys`**: bindgen over `nvcomp.h` batched C API + `libloading` for `nvcomp.dll`; ~9 codec modules but narc needs only `nvcompBatched{Zstd,LZ4}{Compress,Decompress}*` — a small, stable surface |

FFI notes: nvCOMP's low-level batched C API is plain C (device pointers + sizes + stream), trivially bindable; memory management stays on our side via cudarc (`cudaMallocAsync`/pools, pinned host staging buffers). The nvtiff-sys + bindgen pattern from the [GeoTIFFs-to-GPUs blog](https://weiji14.pgs.sh/blog/geotiffs-to-gpus-part-2:-barrels-out-of-bytes-streaming-rust-bits-to-the-gpu/) is a working template. License-wise, our bindings are our code; the proprietary DLL is found at runtime (PATH, pip site-packages, explicit config), never shipped in the repo.

---

## 8. Honest assessment: where GPU wins and loses for an archiver

**Wins**
1. **Batch decompression of independent chunks** — narc's extract path is embarrassingly parallel over chunks; GPU decompression at 5–20+ GB/s exceeds any consumer NVMe, while leaving the CPU nearly idle (directly serves the "low priority, responsive Windows" requirement).
2. **Fast-codec compression** (LZ4/Snappy/zstd-low) when chunk batches are large — GPU zstd ~16 GB/s (Blackwell datacenter; expect a few GB/s on a 5060 Ti) vs ~0.5–1.5 GB/s for multithreaded CPU zstd-3 on a 6-core — *and* the CPU stays free for hashing/CDC/I-O.
3. **Batch entropy coding** — ANS at hundreds of GB/s (dietgpu/nvCOMP) is the single largest GPU advantage; a future narc-specific filter+entropy stage could exploit it.
4. **Domain transforms** — BCn texture preconditioning (Brotli-G showed +10–15%), image delta/planar transforms, float shuffling (dietgpu float mode): throughput-friendly, data-parallel, good fits.
5. **CRC32/verification of GPU-resident batches** (nvCOMP built-in).

**Losses**
1. **Ratio-oriented compression**: large-window match finding (zstd-19, LZMA, bsc's CM stages) is pointer-chasing, branchy, serially dependent — CODAG showed GPU decompression is latency/sync-bound, and compression search is worse. **No viable GPU LZMA-class compressor exists (2026).** GPU codecs land near CPU zstd-1..3 ratios.
2. **Hashing/dedup** (blake3): CPU SIMD wins; see §6.
3. **Small jobs**: CUDA context init (~100–300 ms), kernel launch and PCIe round-trips make GPU pointless below a few hundred MB total.
4. **8 GB VRAM under real desktop load**: browser + apps commonly hold 1–3 GB; narc must budget VRAM explicitly (see roadmap) and degrade to CPU on allocation failure.
5. **Ecosystem risk**: the only maintained library is proprietary NVIDIA-only; every open alternative is archived (dietgpu, Gstd), dormant (Brotli-G), dead (multians, GST, CULZSS) or a paper. A cross-vendor path means writing and maintaining our own WGSL kernels.

---

## 9. Realistic GPU roadmap for Nova Arc

**Architecture principle (Phase 0, do this regardless):**
GPU is an **optional accelerator behind a trait, never a format feature or core dependency**.

```rust
/// Provider chosen at runtime; archive bitstream identical either way.
trait ChunkCodecProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;              // CpuZstd, CpuLz4, GpuNvcomp, GpuWgpu...
    fn budget(&self) -> ResourceBudget;          // bytes of RAM/VRAM it may use
    fn compress_batch(&self, chunks: &[ChunkRef], lvl: Level) -> Result<Vec<CompChunk>>;
    fn decompress_batch(&self, chunks: &[CompChunkRef]) -> Result<Vec<Chunk>>;
}
```

- `.narc` records only the codec ID (zstd/lz4) per chunk — **standard bitstreams only** (nvCOMP's proprietary ANS/Bitcomp/Cascaded formats are banned from the format for now; a weak PC without any GPU must always be able to extract with CPU zstd).
- Runtime probe order: `nvcomp.dll` + CUDA driver present and GPU ≥ sm70 → offer `GpuNvcomp`; else CPU. Cargo feature `gpu` compiles the FFI glue; the default binary still runs everywhere because loading is dynamic (cudarc-style dlopen).
- Scheduler treats the GPU provider as one more worker that consumes chunk batches from the same queue as CPU workers — a slow/absent GPU never blocks progress, and on fast-CPU/weak-GPU machines the CPU workers naturally take most batches.

**Phase 1 — NVIDIA decompress + fast compress (highest value/effort ratio)**
- `narc-nvcomp-sys` (bindgen + libloading) covering batched zstd + LZ4, compress and decompress; cudarc 0.19 for streams/memory/pinned buffers.
- Batch geometry for 8 GB cards: process super-batches of ~256–512 MiB uncompressed per stream, double-buffered (in-flight: input + output + nvCOMP temp ≈ 3× batch), hard VRAM budget default **≤ 2 GiB**, configurable; on `cudaErrorMemoryAllocation` halve batch or fall back to CPU.
- Ship behind `--gpu auto|on|off` (default `auto` = use only if probe succeeds and job ≥ ~256 MB).
- Honest expectation to verify in bench: extract path 2–5× faster than 6-core CPU while CPU load stays near zero; create path with zstd-low ratio ≈ CPU zstd-2..3 at several GB/s.

**Phase 2 — measurement + transforms**
- Public benchmark suite (Silesia + game-asset + photo corpora) CPU vs GPU per chunk size; publish numbers (nobody has honest mid-range-GPU archiver numbers — this is also marketing).
- GPU preconditioning filters where they pay: BCn/texture rearrangement (Brotli-G's spec as reference), float shuffle (dietgpu float mode idea) — output still compressed as standard zstd.

**Phase 3 — cross-vendor experiment (wgpu)**
- Port a **decompressor** to WGSL: start from Microsoft's MIT Zstd compute shader (tuned for ≤ 256 KB chunks — matches narc) and/or GDeflate HLSL. Requires wgpu subgroups (native-only, fine for us).
- Decompression-only is acceptable: cross-vendor users get GPU extract, CPU create. A cross-vendor GPU *compressor* stays out of scope until someone demonstrates one anywhere.

**Phase 4 — research track (optional, narc-experimental codec ID)**
- GPU ANS entropy stage from a vendored dietgpu fork (MIT, archived) or MANS-style portable ANS — only as an opt-in non-default codec, since it breaks the "CPU-extractable everywhere" rule unless we also write a CPU decoder for the same stream.

**Explicit non-goals** (see negative knowledge): GPU blake3/dedup, GPU LZMA-class ratios, DirectStorage runtime integration, Brotli-G adoption, hardware DE reliance.

---

## 10. Sources

- nvCOMP: [docs](https://docs.nvidia.com/cuda/nvcomp/index.html) · [installation](https://docs.nvidia.com/cuda/nvcomp/installation.html) · [release notes](https://docs.nvidia.com/cuda/nvcomp/release_notes.html) · [developer page](https://developer.nvidia.com/nvcomp) · [EULA](https://github.com/NVIDIA/nvcomp/blob/main/LICENSE) · [Benchmarks.md](https://github.com/NVIDIA/nvcomp/blob/main/doc/Benchmarks.md) · [Blackwell DE blog](https://developer.nvidia.com/blog/speeding-up-data-decompression-with-nvcomp-and-the-nvidia-blackwell-decompression-engine/) · [DE FAQ](https://docs.nvidia.com/cuda/nvcomp/decompression_engine_faq.html) · [encode.su thread](https://encode.su/threads/3626-nvCOMP-nVidia-compression-library)
- DirectStorage: [1.1 announcement](https://devblogs.microsoft.com/directx/directstorage-1-1-now-available/) · [1.3](https://devblogs.microsoft.com/directx/directstorage-1-3-is-now-available/) · [1.4 + Zstd](https://devblogs.microsoft.com/directx/directstorage-1-4-release-adds-support-for-zstandard/) · [Tom's HW on 1.4](https://www.tomshardware.com/video-games/pc-gaming/microsoft-debuts-directstorage-1-4-at-gdc-2026-with-zstandard-compression-and-gacl-update-promises-developers-improved-compression-ratios-faster-loading-and-more) · [GDeflate reference](https://github.com/microsoft/DirectStorage/blob/main/GDeflate/README.md) · [NVIDIA GDeflate blog](https://developer.nvidia.com/blog/accelerating-load-times-for-directx-games-and-apps-with-gdeflate-for-directstorage) · [Blackwell DirectStorage test](https://www.tomshardware.com/pc-components/gpus/testing-directstorage-with-gpu-decompression-do-blackwell-gpus-have-the-upper-hand) · [PCWorld adoption post-mortem](https://www.pcworld.com/article/2609584/what-happened-to-directstorage-why-dont-more-pc-games-use-it.html)
- Vulkan: [VK_EXT_memory_decompression proposal](https://docs.vulkan.org/features/latest/features/proposals/VK_EXT_memory_decompression.html) · [VK_NV sample](https://github.com/nvpro-samples/vk_memory_decompression) · [Khronos RTX IO news](https://www.khronos.org/news/permalink/nvidia-ships-vulkan-extensions-to-support-accelerated-rtx-io-on-windows-and-linux)
- AMD: [brotli_g_sdk](https://github.com/GPUOpen-LibrariesAndSDKs/brotli_g_sdk) · [GPUOpen announcement](https://gpuopen.com/news/brotli-g-sdk-announce/) · [Compressonator 4.5](https://gpuopen.com/learn/compressonator-v4-5-improved-brotli-g-compression/)
- Academic/OSS: [dietgpu](https://github.com/facebookresearch/dietgpu) · [hipANS](https://github.com/PAA-NCIC/hipANS) · [multians](https://github.com/weissenberger/multians) + [ICPP'19 paper](https://dl.acm.org/doi/10.1145/3337821.3337888) · [MANS SC'25](https://dl.acm.org/doi/10.1145/3712285.3759825) · [CULZSS](https://web.cs.hacettepe.edu.tr/~aozsoy/papers/2011-ppac.pdf) · [GPULZ](https://arxiv.org/abs/2304.07342) · [CODAG](https://arxiv.org/abs/2307.03760) · [Recoil](https://dl.acm.org/doi/10.1145/3605573.3605588) · [libcubwt](https://github.com/IlyaGrebnov/libcubwt) · [GST repo](https://github.com/GammaUNC/GST) + [paper](https://gamma.cs.unc.edu/GST/gst.pdf) · [Gstd](https://github.com/elasota/gstd) · [GPU FSST VLDB'25](https://www.vldb.org/2025/Workshops/VLDB-Workshops-2025/ADMS/ADMS25-01.pdf) · [GPU-lossless survey](https://github.com/dingwentao/GPU-lossless-compression)
- blake3-GPU: [Blaze-3](https://github.com/Blaze-3/BLAKE3-gpu) · [SYCL blake3](https://github.com/itzmeanjan/blake3) · [Vulkan experiment (Phoronix)](https://www.phoronix.com/news/BLAKE3-Experimental-Vulkan) · [OpenCL issue #136](https://github.com/BLAKE3-team/BLAKE3/issues/136)
- Rust: [cudarc](https://docs.rs/cudarc) · [Rust-CUDA reboot](https://rust-gpu.github.io/blog/2025/01/27/rust-cuda-reboot/) · [Aug 2025 update](https://rust-gpu.github.io/blog/2025/08/11/rust-cuda-update/) · [wgpu](https://github.com/gfx-rs/wgpu) · [wgpu subgroups](https://github.com/gfx-rs/wgpu/issues/5555) · [zarr GPU zstd PR](https://github.com/zarr-developers/zarr-python/pull/2863) · crates.io: `baracuda-nvcomp`, `s4-codec`
