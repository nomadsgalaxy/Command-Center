# Phase 1 decisions: texture contract, layer budget, interim room rule

Status: draft for review, 2026-10-31. This is the "three things get decided and written down"
list from `docs/openxr-plan.md` ("Phase 1"). Nothing here is built. Paths are relative to
`crates/cc-panels/src/` unless they start with `crates/`, `docs/` or a URL. Anything I couldn't
check against the code, the spec or a source is marked **unverified**, and anything I'm unsure
about is written as a question for you to argue with. It sits in docs/ for review; nothing in the code uses it yet.

Sources I read for this:

- The plan, in full (`docs/openxr-plan.md`, sections 1, 2.3, 2.7, 3 and Phase 1 most closely).
- The code: `gpu.rs`, `capture.rs`, `texture.rs`, `back.rs`, `surface.rs`, `grab.rs`
  (`paint_order`), `overlay.rs`, `taskbar.rs`, `plasmabar.rs`, `control/hud.rs`, `main.rs`,
  `config.rs`, and `crates/cc-proto/src/conf.rs`.
- The OpenXR 1.1 spec, fetched from registry.khronos.org/OpenXR/specs/1.1/html/xrspec.html on
  2026-10-31. I cite section numbers where I found them. The per-function man pages turned out to be
  stubs for several of these topics, so the quotes below come from the single-page spec.
- WayVR at commit `ef7e5f6` of https://github.com/wlx-team/wayvr (the plan says `wayvr-org/wayvr`;
  I used the `wlx-team` one that I could clone). Paths below are in that repo.

## 0. The short version

1. **Texture contract.** One OpenXR swapchain per surface that owns a picture. A dmabuf is imported
   once per source buffer and cached (as `capture.rs` caches handles per `BufId` today). Each dirty
   frame, on the main thread, we acquire the swapchain image, copy into it, submit, and release
   (submit-then-release with no CPU wait is the shipped rule; a fence wait before release is a
   spike-only control).
   The source buffer goes back to its producer when the copy's `VkFence` signals, not a tick later.
   PipeWire's implicit fence (`POLLIN`) stays as the first gate, and a `sync_file` exported from the
   dmabuf and waited on by a semaphore is the target for the GPU side. A failed import falls back
   to a staging upload and says so once. A panel's back is a second layer on the front's swapchain,
   so nothing is imported or copied twice.
2. **Layer budget.** Read `maxLayerCount` once at session start. Normally one layer per visible
   surface, sorted ascending by `Surfaces::order`. Over budget: drop backs first, then fold each
   panel's card into its picture, then drop the lowest-priority surfaces, with one log line naming
   what went. A protected set (cursor, controls, the grabbed or hovered panel) is never dropped
   first, a floor of 7 layers bounds the halving after a `LAYER_LIMIT_EXCEEDED`, and a dropped
   surface takes no input.
3. **Room rule.** The room becomes a namespaced id (`openxr:monado`; OpenVR's bare universe ids
   stay as they are, so `home.json` needs no migration). `Session` returns one fixed room per runtime on OpenXR. A
   workspace is the thing that tells layouts apart. Spots then save and restore through the paths
   that already exist for a real universe.

Open questions are in section 5, with the three I most want an answer to first.

## 1. The Vulkan texture contract

### 1.1 What we're replacing

On OpenVR today (plan 2.3, and the doc comment on `Textures` in `texture.rs`):

- `ImportDmabuf` takes its own reference to the buffer. `gpu.rs` closes its fd straight after the
  import (`import_fd`). `capture.rs` imports with a dup of PipeWire's fd and drops the dup with the
  frame.
- The core may write a buffer again "a tick after `SetOverlayTexture` replaced it". `gpu.rs` keeps
  two GBM buffers per panel and `Shared::held` blocks the replaced one for a tick. `capture.rs`
  `Shown::held` keeps the shown buffer and the one before it back from PipeWire.
- Readiness for a PipeWire buffer is `poll(fd, POLLIN, 0)` in `Shown::tick`. If it isn't readable,
  the frame is put back as `newest` for the next tick. Our own GBM buffers are CPU-written between
  map and unmap, so they're ready when the copy returns.
- `UnrefResource` returns at once and nothing says the runtime has let go.
- A panel's back shows the panel's handle (`back::shown`, the `TEX` table in `back.rs`).

The plan is right that these are things SteamVR happens to tolerate, not guarantees. The Vulkan
contract below replaces each with something we can actually check.

### 1.2 Moving parts

- **Sources** (all produce a `Dmabuf` in `texture.rs`, or CPU pixels):
  - RDP and VNC panels: our own linear GBM buffers, two per panel (`gpu.rs` `Buffers::make`,
    `USE_LINEAR | USE_RENDERING`). XRGB8888 for panels, ABGR8888 for CPU-drawn UI cards.
  - PipeWire window and screen streams: KWin's buffers, 2 to 8 of them (`capture.rs` `buffers()`,
    default 4), with a modifier that KWin fixes, or LINEAR if it asks us. `Pixels::Mem` is the
    shared-memory fallback that already exists for streams with no dmabuf.
  - CPU UI cards (taskbar, cards, HUD, Machines, Preferences, Workspace): RGBA from `show_raw`
    today.
- **Sinks:** OpenXR swapchain images, created through `XR_KHR_vulkan_enable2` on the Vulkan device
  we create with the runtime's required extensions.

Threading rule, from the spec and from our own habit: the runtime may touch the session's `VkQueue`
inside `xrBeginFrame`, `xrEndFrame`, `xrAcquireSwapchainImage` and `xrReleaseSwapchainImage`, and
the app has to externally synchronise that queue (OpenXR 1.1, 12.25, "Vulkan 2 Graphics Binding").
So every `vkQueueSubmit` and every swapchain call stays on the main thread, which is the same rule
`gpu.rs` states for OpenVR calls today. The RDP thread keeps writing GBM buffers only.

### 1.3 Lifetime

- **A swapchain belongs to a surface that shows a picture.** It's created lazily on the first
  present, with the format chosen in 1.5 and the source's size. A resize destroys and recreates it.
  WayVR does the same (`wayvr/src/backend/openxr/overlay.rs`, `ensure_swapchain_acquire`) and puts
  the old one on a retirement queue until the GPU work that touched it is done
  (`wayvr/src/backend/openxr/swapchain.rs`, `SwapchainRetirementQueue`, which arms a completion
  marker per retired swapchain). We should do the same, with our fences instead of their marker.
- **An imported source buffer is `{ VkImage, VkDeviceMemory, dup'd fd consumed by the allocation }`,
  keyed by the buffer's identity.** For PipeWire that's `BufId` (address plus generation, so a
  renegotiation can't match a stale one, `capture.rs`). For our GBM buffers it's the `(panel, k)`
  pair. The backend owns the Vulkan objects. The core still owns the buffer and the fd it got.
- **The fd.** `vkAllocateMemory` with `VkImportMemoryFdInfoKHR` takes ownership of the fd it's given
  on success, so we pass a dup. WayVR does exactly this (`wgui/src/gfx/raw.rs`, `import_dmabuf`,
  around the `try_clone` before `allocate_memory`). That keeps `gpu.rs` and `capture.rs` unchanged
  in what they hold, which is the point of the `Textures` trait: `import(&Dmabuf)` doesn't take the
  caller's fd, same as now.
- **Destroying an import** (`release`) is deferred until every submitted copy that reads it has
  finished. The backend keeps, per import, the fence of the last copy that used it, and destroys
  from a once-per-tick sweep. `release` returns at once, as now, and the sweep is why nothing is
  freed under the GPU. This is the part OpenVR never gave us ("`release` returns at once, with no
  signal when the runtime has let go").
- **A source buffer going away while a copy is in flight** is the same case. `capture.rs`'s `gone`
  list already says which `BufId`s PipeWire removed. The backend moves those imports to the
  deferred list instead of destroying them.

### 1.4 Reuse

- One import per source buffer for that buffer's life. A PipeWire stream cycles through its 2 to 8
  buffers, so after the first lap every frame hits the cache (`Shown::handles` already does this).
  A panel's two GBM buffers are imported once, at `Buffers::make`.
- One swapchain image per dirty frame. A panel that didn't change doesn't acquire anything. We
  re-submit its last released image. The spec says all layers to be drawn must be submitted every
  `xrEndFrame`, and an omitted layer isn't drawn (OpenXR 1.1, 10.6 "Compositing"), and that a layer
  needs a swapchain with a released image (`XR_ERROR_LAYER_INVALID` otherwise, `xrEndFrame`
  man page). So a released image stays referencable. WayVR relies on this: an overlay that doesn't
  re-render is "showing stale frame" and just re-presents its swapchain
  (`wayvr/src/backend/openxr/mod.rs`, the render loop).
- Pipeline objects, descriptor sets and the command pool are created once per format, not per
  frame.
- Modifier handling: query the format and modifier with
  `vkGetPhysicalDeviceImageFormatProperties2` plus `VkPhysicalDeviceImageDrmFormatModifierInfoEXT`
  before creating the image, and create it with `VkImageDrmFormatModifierExplicitCreateInfoEXT` and
  the plane layout (offset, stride) we already carry in `Dmabuf`. WayVR does the same
  (`wgui/src/gfx/raw.rs`, `import_dmabuf` and `query_dmabuf_image_support`; its
  `wayvr/src/graphics/dmabuf.rs` also advertises its supported modifiers to the capture side).
  `capture.rs` today offers PipeWire one modifier. Question 3 asks whether we should offer the
  set the Vulkan device can import instead.

### 1.5 Format, and copy versus draw

The plan says `vkCmdCopyImage`. Two things make that not automatic:

- **Byte order.** Our buffers are BGRx (DRM XRGB8888, `capture.rs` and `gpu.rs`), and UI cards are
  RGBA (`ABGR8888`). `vkCmdCopyImage` between size-compatible formats copies bytes, so a B8G8R8A8
  source copied into an R8G8B8A8 swapchain swaps red and blue. The runtime decides which formats
  `xrEnumerateSwapchainFormats` offers, and I don't know what Monado or WiVRn offer first
  (**unverified**).
- **The X byte.** XRGB's fourth byte isn't guaranteed to be 255. If we leave
  `XR_COMPOSITION_LAYER_BLEND_TEXTURE_SOURCE_ALPHA_BIT` off, the layer is treated as opaque and the
  byte doesn't matter (OpenXR 1.1, 10.6.1, layer flags). Panels set it off. Cards that have real
  alpha set it on.

Proposal: pick the swapchain format per surface kind from the enumerated list. For opaque panels
prefer a B8G8R8A8 format so a plain `vkCmdCopyImage` works. For cards prefer R8G8B8A8. If the
runtime offers neither match, use a small draw pass (sample the imported image, write the
swapchain image), which is how WayVR does every frame
(`wayvr/src/overlays/screen/capture.rs` renders the imported image through a pipeline;
`graphics/dmabuf.rs` `fourcc_to_vk` maps XRGB to `B8G8R8A8_UNORM`). The draw pass is also where a
quarter-turned panel (`PanelFacts.vert`, the vertical curve) and a crop could be done instead of
in the layer. I don't want to build the draw pass in phase 1 unless the spike shows a runtime that
needs it.

sRGB versus UNORM (Grok, #380): desktop and KWin pixels are sRGB-encoded bytes, and a plain
`vkCmdCopyImage` copies bytes without converting them. A runtime treats a UNORM swapchain's values
as linear, so copying into `B8G8R8A8_UNORM` would show every panel washed out. Copying the same bytes
into a `_SRGB` swapchain image is allowed (the formats are size-compatible) and displays them as
intended. So the pick prefers the `_SRGB` variants (`B8G8R8A8_SRGB` for panels, `R8G8B8A8_SRGB` for
cards), and the spike's corner-colour check compares against expected sRGB values, so a UNORM pick
fails the test instead of looking a bit off. **Unverified** against the 1.1 spec's colour-space
text; the spike settles it (Question 4).

### 1.6 Readiness and fencing

Two separate waits, which OpenVR let us blur together.

**Wait 1: the producer has finished writing the source.**

- *Today.* PipeWire gives us the buffer after the producer queued it, and `Shown::tick` checks
  `POLLIN` on the dmabuf fd. For a dmabuf, `poll` for `POLLIN` becomes ready when the buffer's
  write fences have signalled (the kernel's implicit sync: `dma_resv` exclusive fence). That's the
  "implicit fence" the plan refers to. It's a CPU check at one moment.
- *What Vulkan needs.* A Vulkan device that imports a dmabuf doesn't get implicit synchronisation
  for free. The spec for the memory import says nothing about the producer's fences. We need to
  turn the implicit fence into something a queue submit can wait on, or make sure it's already
  signalled before we submit.
- *Options.*
  - **A (baseline, what we have):** keep the `POLLIN` check, and only record the copy if the
    buffer is readable now. Because PipeWire has handed us the buffer exclusively, the producer
    can't start a new write until we give it back, so "readable now" stays true until then. This
    is exactly the reasoning the OpenVR path uses. It adds nothing to the submit. **Unverified**:
    whether KWin's screencast always attaches a fence (it works on OpenVR today, which suggests
    yes, but SteamVR's importer might wait by itself).
  - **B (target):** export the fence with `DMA_BUF_IOCTL_EXPORT_SYNC_FILE` on the dmabuf (Linux
    6.0 and later; flag `DMA_BUF_SYNC_READ` for "fences I must wait on before reading"), import
    that sync file into a `VkSemaphore` with `VK_KHR_external_semaphore_fd` (`SYNC_FD` handle
    type, temporary import), and put the semaphore in the copy's `pWaitSemaphores`. Then the GPU
    waits and the CPU never blocks. This is what a compositor does. I didn't find it in WayVR (I
    grepped `wayvr`, `wgui` and `wlx-capture` for `sync_file`, `SYNC_FD` and `poll(`; the only
    `poll` was an unrelated input-capture one), so WayVR seems to rely on the driver or the
    producer being done (**unverified**, absence of a grep hit isn't proof).
  - **C:** `poll` for `POLLIN` with a short timeout and skip the frame if it isn't ready. That's A.
  - Choose **A in the spike, B as phase 1's target** if A shows tearing or stale pixels on any
    target GPU. The spike's exit test includes a deliberately slow producer for that reason.
- *For our own GBM buffers* (RDP and VNC): they're written by the CPU between `gbm_bo_map` and
  `gbm_bo_unmap`, so there is no producer fence. **CPU coherency is a requirement of this contract,
  not a footnote.** The kernel's rule is that CPU access through an mmap of a dmabuf has to be
  bracketed by `DMA_BUF_IOCTL_SYNC` with `DMA_BUF_SYNC_START` and `DMA_BUF_SYNC_END`
  (docs.kernel.org/driver-api/dma-buf.html, "CPU Access to DMA Buffer Objects"). Does GBM already
  do it? I checked Mesa at `b27d00d` (a sparse checkout of `src/gbm` and `src/gallium/frontends/dri`):
  - `grep -rn 'DMA_BUF_IOCTL_SYNC\|DMA_BUF_SYNC'` over both trees found nothing. Not in
    `src/gbm/backends/dri/gbm_dri.c`, and not in the dumb-buffer helper in `gbm_driint.h`.
  - `gbm_dri_bo_map` doesn't mmap the dmabuf fd. For a normal (non-dumb) buffer it calls
    `dri2_map_image`, a gallium transfer map on the driver's own mapping of the buffer.
    `gbm_dri_bo_unmap` calls `dri2_unmap_image` and then `dri_flush(..., __DRI2_FLUSH_CONTEXT)`
    ("Not all DRI drivers use direct maps. They may queue up DMA operations on the mapping
    context"). So GBM's own guarantee is a context flush, not a dmabuf sync.
  - So the kernel rule is about a path we don't take (mmap of the dmabuf fd), and GBM's map
    doesn't call the ioctl. Whether the driver's own map is coherent for a later Vulkan read
    through the imported dmabuf is the driver's business, and I couldn't confirm it for radeonsi,
    anv or Turnip from this source (**unverified**).
  - **Contract:** `gpu.rs`'s write path (`write_frame` and `show_raw`'s replacement, between map
    and unmap) is bracketed by `DMA_BUF_IOCTL_SYNC` `START|WRITE` and `END|WRITE` on the buffer's
    exported fd (`gbm_bo_get_fd`, which `Buffers::make` already has before it hands the fd over).
    Because GBM doesn't issue it itself (above), there's no doubling up. Cost: two ioctls per
    write, no copy. If a later Mesa starts doing it inside `gbm_bo_map`, the spike's check on item
    2 below catches nothing wrong (a second sync is harmless), and we can drop ours then.
  - The same ioctl is required anywhere we `mmap` a dmabuf fd ourselves: the PipeWire fallback in
    1.10 if it ever reads a dmabuf on the CPU, and the VNC and RDP staging paths if they were to
    mmap one. Staging `VkBuffer` memory we allocate through Vulkan isn't a dmabuf, so it needs
    `vkFlushMappedMemoryRanges` only if the chosen memory type isn't `HOST_COHERENT`; we choose
    one that is.
- *For SPA explicit sync.* Newer PipeWire has `SPA_META_SyncTimeline` for explicit sync.
  **Unverified**: whether KWin's screencast or our PipeWire version offers it. If it does, that's
  option B without the ioctl.

**Wait 2: our copy has finished reading the source, and (separately) writing the swapchain image.**

- Each copy is submitted with a `VkFence`. The backend keeps `{fence, source buffer id}` in a
  small in-flight list.
- Barriers around the copy, from what WayVR does for imported images
  (`wgui/src/gfx/raw.rs` near 2275: acquire from `VK_QUEUE_FAMILY_FOREIGN_EXT`, transition from
  `GENERAL` rather than `UNDEFINED` so the producer's contents are preserved, and a matching
  release to `FOREIGN` at the end). This needs `VK_EXT_queue_family_foreign`. We do the same:
  - source: `FOREIGN_EXT` to our family, `GENERAL` to `TRANSFER_SRC_OPTIMAL`, copy, then back to
    `FOREIGN_EXT` and `GENERAL`.
  - destination: the runtime guarantees it's in `COLOR_ATTACHMENT_OPTIMAL` layout and owned by our
    queue when `xrWaitSwapchainImage` returns, and it reads it back as the same layout and
    ownership on release. We have to transition to `TRANSFER_DST_OPTIMAL` and back
    (OpenXR 1.1, 12.25.4 "Swapchain Interactions": "The application is responsible for
    transitioning the swapchain image back to the image layout and queue ownership that the
    OpenXR runtime requires"). The swapchain needs `TRANSFER_DST` usage, so the create call asks
    for `XR_SWAPCHAIN_USAGE_TRANSFER_DST_BIT`, not only the `COLOR_ATTACHMENT | SAMPLED` that
    WayVR uses (`swapchain.rs`, `create_swapchain`) because WayVR draws instead of copying.
- *Before `xrReleaseSwapchainImage`: which rule is the shipped one.* There are two, and only one
  of them ships.
  - **Shipped contract: submit, then release, no CPU wait.** The copy has to be **submitted**
    (`vkQueueSubmit` returned success) before release. We do not call `vkWaitForFences` in the
    frame path. Fences are only polled (`vkGetFenceStatus`, 1.7). The runtime shares our `VkQueue`
    and the spec says the image must be in the stated layout and owned by the queue on release
    (OpenXR 1.1, 12.25.4). Queue submission order gives an execution order on one queue, so
    submit-then-release should be enough, but I haven't found the sentence that says a runtime
    may rely on it (**unverified**).
  - **Spike-only control: wait on the copy's fence, then release.** The spike runs both. The
    blocking variant is the control that tells us whether a corruption seen with submit-only is a
    sync problem or something else. It is not shipped.
  - **If the spike shows a runtime needs the wait:** that runtime gets a named per-runtime mode,
    "wait before release", with a bound (proposal: 4 ms). On a timeout the image is held, not
    released: the copy is submitted but not known finished, so releasing would defeat the mode. The
    layer keeps the last released image, the wait is retried next frame, and after 30 timeouts in a
    row the surface goes the way of 1.7's wait-timeout row (destroy the swapchain, Return the
    source, log once). Muse, #385.
    That is an exception to "no `vkWaitForFences` in the frame path", it is written down as one,
    and it's decided per runtime from the spike's result, not by default.

**When a source buffer may be written again.** When the copy's fence has signalled. Not "a tick
later" and not "a frame after it was shown". The swapchain image holds the pixels now, so the
source isn't needed once the copy is done, even though the picture hasn't reached the display
yet. Concretely:

- `gpu.rs`: `Shared::held` stops meaning "SteamVR may still read it" and becomes "a copy is in
  flight". `next_tick()` is replaced by a once-a-tick `poll_fences()` that clears `held` for
  buffers whose fence has signalled and pokes the RDP thread (`rdp::poke`), as `present` does now
  at `next_tick`. The two-buffer scheme stays (the RDP thread can write buffer B while buffer A's
  copy is in flight).
- `capture.rs`: `Shown::held: [Option<BufId>; 2]` and the "shown plus the one before it" rule go.
  `cap.give_back(key, buf)` is called when the copy's fence signals. A frame that's superseded
  before its copy is submitted goes back at once (as the `Some(_)` branch of the readiness check
  does today). Fewer buffers are held back from PipeWire than on OpenVR, which should help KWin
  when it has few buffers.
- If a frame is dropped because the swapchain image wasn't available, or the import failed, the
  buffer goes back at once. Nothing we hold on to should be unpaired.

**Which fence-signalled path wakes the main loop?** The main loop parks between ticks (plan,
risk 3, `vr.rs` `MAIN`/`wake`). On OpenXR it won't park while a session runs, so polling fences
once per frame is free. If we ever park again, a fence wait needs a wake-up (Question 6).

### 1.7 Release, and who holds what

**Rules that apply everywhere:**

- The frame path never blocks on the GPU. A once-per-frame sweep on the main thread polls
  `vkGetFenceStatus` for every in-flight copy and every deferred import. `vkWaitForFences` and
  `vkQueueWaitIdle` are allowed in teardown only (the table below), plus the per-runtime
  wait-before-release mode if the spike makes us add one (1.6).
- `Textures::release(t)` marks an import for destruction after its last fence. It returns at once,
  as now.
- A surface holds **at most one acquired swapchain image** at a time. It never acquires a second
  until the first is released or its swapchain is destroyed.
- A released image stays referencable (1.4), so a surface can always draw its last good picture
  while a new one is being made.
- The source buffer states are: **Producer** (the producer or the RDP thread may write it),
  **Ours** (we hold it as the newest frame, nothing in flight), **In flight** (a submitted copy
  reads it), and **Returned** (back to Producer). It becomes Returned when the copy's fence
  signals, or at once if no copy was ever submitted for it.

**What happens in each case.** "Image" is the surface's swapchain image. Numbers in the
"Proposed" column are starting values for review.

| Case | Swapchain image | Source buffer | When each is released |
|---|---|---|---|
| **Normal** | `xrAcquireSwapchainImage`, `xrWaitSwapchainImage` (bounded, see next row), barrier to `TRANSFER_DST`, copy, barrier back to `COLOR_ATTACHMENT_OPTIMAL`, `vkQueueSubmit` with a fence, then `xrReleaseSwapchainImage` in the same frame. Held for the length of that sequence only. | Ours, then In flight at submit. | Image: released right after the submit succeeds. Source: Returned when the fence signals (next sweep after it does). A frame superseded before its submit is Returned at once. |
| **`xrWaitSwapchainImage` times out** (`XR_TIMEOUT_EXPIRED`) | Acquired but not waited. It can't be released (release without a wait is `XR_ERROR_CALL_ORDER_INVALID`, `xrReleaseSwapchainImage` man page), so we keep holding it and wait again next frame. We never call acquire again meanwhile. | Stays Ours (the newest frame). A newer frame replaces it and the older one is Returned at once. | The wait is bounded, not `XR_INFINITE_DURATION` (WayVR waits forever, `swapchain.rs` `acquire_wait_image`). Proposed: 2 ms per frame. The surface keeps drawing its last released image. After 30 consecutive timeouts (about 0.3 s at 90 Hz): destroy the swapchain (this drops the held image with it), Return the source, log once, recreate on the next frame. |
| **Submit fails** (`vkQueueSubmit` returns an error) | Acquired and waited, but not written, so releasing it would make garbage the "last released" image. We do **not** release it. The layer keeps the previously released image. | Ours (never In flight). | Retry the copy next frame, up to 3 times. After the third failure: destroy the swapchain with the image held, Return the source, log once with the `VkResult`, recreate lazily. `VK_ERROR_DEVICE_LOST` skips the retries and is treated as session loss (last row). **Unverified:** that every runtime accepts a layer that references a swapchain which currently has an acquired image. The spec text I found says a layer references the last released image, not the acquired one. |
| **Resize** (new size from the source) | A new swapchain is created. The old swapchain keeps being submitted, at its old size, until the new one has released its first image, then the layer switches in one frame. If the old one holds an acquired image it is released only if already waited, else left to die with the swapchain. | In-flight copies on the old swapchain finish as normal. The old GBM pair or old PipeWire buffers are Returned/freed as their fences signal. | The old swapchain goes on a retirement list and is destroyed once its last fence has signalled (WayVR's `SwapchainRetirementQueue` does this with a completion marker). This replaces "frees both at once, the shown one included" (plan 2.3). Nothing is freed on resize without a fence. |
| **`STOPPING`** (session ending, the app may restart it on `READY`) | Stop acquiring. For each surface: if an image is acquired and waited, release it (after the in-flight submit, which already happened). If acquired and not waited, leave it. Then `xrDestroySwapchain` for every swapchain. | Ours: Returned at once. In flight: Returned after the single teardown `vkQueueWaitIdle`. | Order: stop the frame loop, `vkQueueWaitIdle` (teardown only, bounded by the driver), Return all sources, destroy deferred imports and every import, destroy swapchains, then `xrEndSession`. On `READY` again, swapchains and imports are re-created lazily on the next present. The core's buffers were never touched. |
| **`LOSS_PENDING`, `EXITING`, `InstanceLossPending`, or `VK_ERROR_DEVICE_LOST`** | Don't release anything and don't wait on the queue for longer than a bounded timeout (a lost device may never idle). Destroy the swapchains. | All sources Returned immediately, whatever their fences. In-flight copies are abandoned with the device. | Destroy imports and Vulkan objects without waiting, then the XR session and instance. The backend is dropped. A fresh backend is built when the runtime is back (plan 2.6, `Quit`), and every surface presents again from nothing. Producers aren't left holding a buffer we never gave back. |

Things this table pins that the earlier draft left loose:

- "Release any acquired image" is gone. The rule is: release after a successful submit, never
  after a failed one, never before a wait, and on teardown only an image that has been waited.
- Source buffers are Returned on fence signal, or at once on every path that never submitted. There
  is no path where a source is held with no fence and no owner.
- Every timeout and retry count above is a proposal. The spike's resize and teardown steps (1.11)
  exercise each row.

### 1.8 Sharing, and a panel's back

A swapchain can be referenced by more than one layer in a frame, and only the image from the last
release is referenced (`xrEndFrame` man page: "only one image index is implicitly referenced per
frame, the one corresponding to the last call to xrReleaseSwapchainImage"; "a swapchain can be
referenced in `XrFrameEndInfo` multiple times within a frame, for example with different image
rectangles"). So:

- **A back is a second layer on the front's swapchain.** No second import, no second copy, no
  `TEX` table of raw handles in `back.rs`. The trait becomes `share(front: SurfaceId, back:
  SurfaceId)`, which makes `back`'s layer point at the front's swapchain, and `back::shown`
  goes away on OpenXR.
- **A quad is one-sided.** "Only front face of the quad surface is visible; the back face is not
  visible and must not be drawn by the runtime" (OpenXR 1.1, 10.6.3, quad layers). So the back
  needs its own layer anyway, posed half a turn round. That's what OpenVR does too.
- **Mirroring and dimming.** The OpenVR back is mirrored ("seen through the screen") and dimmed
  (`ALPHA` 0.3 and `DIM` 0.45 in `back.rs`). On a shared swapchain:
  - Dim and fade: `XR_KHR_composition_layer_color_scale_bias` on the back layer, where the runtime
    has it. SteamVR's OpenXR runtime doesn't (plan, section 1). Monado and WiVRn: **unverified**.
    WayVR uses it when present and otherwise doesn't fade (`overlay.rs`, `try_update_color_scale_bias`).
  - Mirroring: a half-turned quad shows the picture un-mirrored, as you'd see a poster from behind
    if it were printed on both sides. I don't see a way to flip a quad's picture without a second
    swapchain image (Question 2). Whether the spec lets a quad take a negative size, I haven't
    found; I wouldn't rely on it.
  - Without colour scale and bias, a back gets either its own tiny dim swapchain (the plain sheet
    `FILL` the code already uses for pictures with no handle) or no dimming. That is the same
    fallback `back.rs` has for a panel without a texture.
- **A dmabuf on two panels** (a window shown by two surfaces) isn't a case today. If it comes up,
  it's two swapchains and two copies, one import. The import cache is keyed by buffer, not
  surface, so it falls out.

### 1.9 CPU-drawn UI cards through a staging copy

Replaces `show_raw`/`SetOverlayRaw` (`gpu.rs`, `grab.rs`, `back.rs`, `laser.rs`):

- One persistent host-visible, host-coherent `VkBuffer` per card surface, sized to the card. The
  card's RGBA is `memcpy`'d into it (the whole picture, the way `show_raw` does now; its
  `ponytail:` comment about patching rects stays true).
- On a present: acquire and wait on the swapchain image, `vkCmdCopyBufferToImage`, barrier,
  submit with a fence, release.
- The buffer is reusable when the fence signals, and a second picture that arrives in the
  meantime is held as `pending` (the same idea as `Raw::pending` in `gpu.rs`) and goes up on the
  next frame. That replaces `raw_tick` and the "old pair for a tick" resize rule.
- The SetOverlayRaw blink doesn't exist here, because the swapchain image isn't shown until we
  release it and the runtime only composes the last released one.
- Cards that don't change don't acquire. The 4x4 laser-mode picture isn't a surface on OpenXR.

### 1.10 A failed import falls back to a staging upload, and says so once

When can an import fail? Any of: the format and modifier combo isn't importable by the device
(`vkGetPhysicalDeviceImageFormatProperties2` says no), `vkGetMemoryFdPropertiesKHR` gives memory
type bits that don't intersect the image's, `vkAllocateMemory` or `vkBindImageMemory` fails, or the
device can't do the extensions at all (`VK_EXT_external_memory_dma_buf`,
`VK_EXT_image_drm_format_modifier`, `VK_EXT_queue_family_foreign` all missing). WayVR checks each
of these and returns an error with the reason (`wgui/src/gfx/raw.rs`, `import_dmabuf`).

The fallback, per source kind:

- **PipeWire stream:** the capture side already supports shared memory (`Pixels::Mem`,
  `SPA_DATA_MemFd`). On the first failed import for a stream, tell `capture.rs` to offer
  shared memory only for that stream (a new `Cmd`, next to `Cmd::Rate`), and renegotiate.
  Until then, hand the buffer back at once and show the previous picture. Staging upload then
  works as for cards (1.9), at the cost of a CPU copy by KWin or PipeWire. The existing code
  comment ("SetOverlayRaw refuses past ~1920x1080") is OpenVR's limit and doesn't apply, since
  Vulkan staging has no such cap beyond `maxImageDimension2D`.
- **RDP and VNC:** these buffers are ours, so skip GBM and write into a staging `VkBuffer`
  directly. `gpu.rs::write_frame` already works from a CPU pointer and stride, so the RDP thread
  would write into mapped staging memory. The two-buffer scheme and `held` rule are the same
  with `fence` for `held`.
- **Logging:** once per source and per reason, in the form
  `"<name>: dmabuf import failed (<reason>), using staging upload"`. Use the pattern in
  `capture.rs` (`self.imported` bool, so the first success or failure is printed once) but keep
  it per source and per reason so a later different failure still shows. The `surfaces` control
  dump gets an `upload=dmabuf|staging` field so a reviewer can see it without the log.
- **Don't retry every frame.** `Buffers::make` retries once a second (`tried`). Same here, per
  source.

### 1.11 The spike and its exit test

Same shape as the plan's ("repeated acquire, present and release, then a resize, a reconnect and
a teardown, on each target GPU and runtime"). I'd write it as pass/fail criteria so the estimate
for this slice can be made from it:

1. **Setup.** A standalone binary (not in cc-panels). Vulkan device through `XR_KHR_vulkan_enable2`
   on the runtime under test, an overlay session if `XR_EXTX_overlay` is there (else a scene
   session, for the spike only), one panel-sized swapchain (3840x1080), one card-sized
   (1024x512).
2. **Picture.** A test source that fills a known pattern (corner colours, a diagonal, a stripe
   with alpha 0 in the X byte) as (a) a GBM linear buffer written on the CPU, (b) a PipeWire
   dmabuf from a real KWin screencast, (c) a GPU-rendered dmabuf with a deliberately slow producer.
   Expected: colours correct (no red/blue swap), no black or torn frames in 10,000 presents, X-byte
   garbage doesn't show.
3. **Cycle.** 10,000 acquire, copy, submit, release, `xrEndFrame` cycles. With the Khronos
   validation layer and synchronisation validation on, zero errors. Source buffers are rewritten
   the instant their fence signals, and a checksum of the swapchain image, read back after the
   copy's fence and before release, matches the pre-rewrite frame (this is the test that "a tick
   later" isn't needed; the compositor's output can't be read back, so the image is what's
   checked).
4. **Negative control.** Rewrite a source before its fence signals on purpose and confirm the test
   sees corruption, so we know the check works.
5. **Resize.** 100 random resizes of the swapchain and the source, no leak and no use-after-free.
6. **Reconnect.** Drop the PipeWire stream and reopen it 100 times (buffers are renegotiated, the
   `BufId` generation changes). A stale give-back never reaches a new buffer.
7. **Teardown.** End the session mid-frame (acquired, not released) and exit. Count open fds in
   `/proc/self/fd` and device memory (`VK_EXT_memory_budget`) before and after: unchanged.
8. **State table rows (1.7).** Inject an `xrWaitSwapchainImage` timeout, a failed submit, a
   resize while a copy is in flight, a `STOPPING` and a device loss, and check each row of the
   table: nothing leaked, nothing released that shouldn't be, every source Returned. Three rows
   destroy a swapchain that still has an acquired image (the timeout, the failed submit, and
   `STOPPING`'s not-waited case). I haven't found the sentence that allows that (**unverified**,
   Muse #385), so this step asserts the destroy itself: no runtime error, and validation clean. If
   a runtime refuses it, those rows wait and release the image first (after a bounded wait), and
   the table changes to say so.
9. **Failure path.** Force an import failure (an unsupported modifier) and confirm the staging
   fallback shows the same picture, with one log line.
10. **Where.** The Vulkan half (items 2 to 9 without the XR calls, copying into a plain
   `VkImage`) runs on the Frame's Turnip stack, since the Frame can't run Monado. The XR half runs on a
   PC with Monado's simulated HMD, and WiVRn with a Quest if one's available (plan 4, Phase 1).
   Targets: AMD (radv) and Intel (anv) on x86, Turnip on aarch64, and whatever the Quest-host PC
   has. I don't know which Mesa versions the Frame's SteamOS ships for Vulkan dmabuf import
   (**unverified**).

Pass means all of 2 to 9 on each target. A target that fails item 3 on A-style fencing moves to
the sync_file path before we call the spike done.

## 2. The layer budget

### 2.1 What the spec promises

- `XrSystemGraphicsProperties::maxLayerCount` is "the maximum number of composition layers
  supported by this system", and the runtime "must support at least
  `XR_MIN_COMPOSITION_LAYERS_SUPPORTED`", which is 16 (OpenXR 1.1 man pages for both).
- `xrEndFrame` returns `XR_ERROR_LAYER_LIMIT_EXCEEDED` if `layerCount` exceeds it. It also may fail
  if it "cannot composite the layers due to resource constraints".
- Layers are drawn "in the same order as they are specified in via XrFrameEndInfo, with the 0th
  layer drawn first. Layers must be drawn with a 'painter's algorithm,' with each successive
  layer potentially overwriting the destination layers whether or not the new layers are virtually
  closer to the viewer" (OpenXR 1.1, 10.6). So depth plays no part. Array order is the whole
  story.
- With `XR_EXTX_overlay`, the main session's layers are composited first and the overlay
  session's after, with `sessionLayersPlacement` ordering overlay sessions among themselves
  (13.1.2). Our layers are always on top of the game's. WayVR passes `5` as the placement
  (`wayvr/src/backend/openxr/helpers.rs`, `create_overlay_session`), a magic number I'd rather
  not copy without a reason (Question 7).
- **Unverified:** whether the 16 (or `maxLayerCount`) is per session or shared with the main
  session's layers in Monado's multi-client compositor. If shared, a game that uses 10 leaves us 6.
  Read `maxLayerCount` once, but plan for a smaller number than it says. WayVR doesn't read it
  at all (I grepped `maxLayerCount`, `max_layer` and `layer_count` over the repo; only unrelated
  Vulkan array layers matched), and submits one or two layers per overlay, so with enough overlays
  it would hit the error.

### 2.2 Counting what we have

From `grep` of `surface::create` and the `Surfaces::order` calls in the code. A surface shown
counts once, and a layer is only submitted for a visible one with a picture.

| Surface | Made in | Count | Order |
|---|---|---|---|
| Panel pictures (remote monitors) | `main.rs:192` `Panel::new` | one per viewer, plus `SPARES` = 2 (`main.rs:130`) | `10 + rank*4 + 1` (`grab.rs` `paint_order`, ~1985-1990) |
| Panel pictures (window panels) | same, `windows::SLOTS` = 16 (`windows.rs:32`) | up to 16 | same |
| Cards (frame, bar, tab) | `grab.rs:805` `create` | one per panel slot (viewers + 2 + 16), plus one per Extra | `10 + rank*4` |
| Backs | `back.rs:249` | at most `BACKS` = 8 | same as the front's (`back.rs:181`, `order_of(front)`), so equal |
| Theater backdrop | `grab.rs:1031` | 1 | `10 + under*4 - 1` while theater is on |
| Taskbar frame | `taskbar.rs:735` | 1 | 190 (`taskbar::SORT`) |
| Plasma's bar and popups | `plasmabar.rs:347` | 1 (a crop of the output, popups included) | 191 |
| Taskbar's card | `grab.rs` (comment at `control/ui.rs:26` says 192) | 1 | 192 |
| Machines, Preferences, Workspace | `machines.rs:1332`, `prefs.rs:308`, `workspace.rs:564` | 1 at a time (`extra_taken`), plus its card | 193 (`ui::SORT`) |
| Key strip | `osk.rs:259` | 1 | 193 |
| Scan HUD marks | `control/hud.rs:48` | 1 | 195 |
| Scan HUD strip | `control/hud.rs:47` | 1 | 196 |
| Cursor | `main.rs:1045` | 1 | 200 |
| Laser-mode overlay | `laser.rs:394` | OpenVR only | not submitted on OpenXR |
| Dashboard tab | `control/dashtab.rs:280` | OpenVR only (`create_dashboard` is `None`) | not submitted |

Typical day: 3 monitors and 2 windows is 5 pictures + 5 cards = 10, plus the taskbar, its card,
Plasma, and the cursor = 14. Add two backs and the HUD and it's 17. Add a Machines window and its
card and it's 19. The worst case is far over (16 windows + viewers + 2 spares, each with a card,
plus 8 backs is 50-plus). Hidden or minimised panels aren't submitted, so the real figure is the
visible set. The point stands: 16 is easy to pass, which is why the plan said to decide this before
building.

### 2.3 Which surfaces get a layer and which share

Rule: **a surface has its own layer unless it's coplanar with another and always drawn with it, in
which case it can share a swapchain with that one when we're short.** Applied:

- **Own layer, always:** each panel picture, the taskbar frame, Plasma's bar, the cursor, the HUD
  strip, the HUD marks, the theater backdrop, an Extra window, the key strip.
  Reason for each: they have different poses or anchors (Room, Head, Parent) and different update
  rates (video versus UI), so merging them would force a redraw of the larger swapchain for every
  video frame.
- **Own layer by default, mergeable under pressure:** a panel's card with its picture. The card
  sits 2 mm behind the picture and wraps it (`back.rs`, `BEHIND` comment), they share one
  `Placement` (`PanelFacts`), and `paint_order` already treats them as one group ("its card, then
  its picture, then its bar"). Merged, the panel is one layer whose swapchain is the card's size
  and the picture is copied (or drawn) into its middle. Cost: the swapchain is bigger and a video
  frame becomes a draw or copy into an offset rect, not a whole-image copy, and a card redraw
  must re-copy the last video frame or keep it separately. I'd do this fold only past budget.
  (**Unverified**: that picture and card geometry always line up; curved and "turned" panels need
  checking.)
- **Own layer, no swapchain of its own:** a back (1.8). It costs a layer, not memory.
- **Never a layer on OpenXR:** laser-mode overlay, dashboard tab, thumbnails.
- **Can't merge:** HUD strip (Head anchor) with HUD marks (Room anchor). Plasma's bar with the
  taskbar frame (separate source, its own texture and bounds).

### 2.4 Order mapping

- `Surfaces::order(s, n)` is SteamVR's sort order: higher is drawn over lower (the cursor is 200,
  the taskbar 190, panels 10 and up). OpenXR draws index 0 first and later layers over earlier
  ones, with no depth (spec quote above). So **submit layers sorted ascending by `order`.** The
  array is the paint order.
- Ties are real: a back has the same order as its front (`back.rs:181,219`), and `paint_order`
  gives a card `base` and its picture `base + 1` so those are distinct. Tie-break by `SurfaceId`
  ascending, so the array is deterministic. For front and back, only one of the pair faces the
  viewer, so the tie doesn't show.
- This is a place where OpenVR was looser than OpenXR: `plasmabar.rs:355` says "in front of the
  taskbar's frame, in case depth isn't used", i.e. we weren't sure SteamVR sorts by order. On OpenXR
  it's guaranteed, which is a plus. The risk is the opposite: if `paint_order` has a wrong
  order for overlapping panels, the user sees a farther panel over a nearer one with no depth to
  rescue it. `paint_order` is already a back-to-front, view-overlap sort (`in_front`, with
  distance as the tie-break and for cycles), which is the right input. WayVR instead sorts every
  frame by distance from the head, farthest first, with its `z_order` subtracted as a tie-break
  (`wayvr/src/backend/openxr/mod.rs`, the "Layer composition" section: `dist_sq` plus
  `100 - z_order`, `layers.sort_by(|a, b| b.0.total_cmp(&a.0))`). Ours is better for wide curved
  panels, which a centre distance gets wrong, and `paint_order` runs ~4 times a second, so a
  fast head turn near a crossing could look wrong for up to 230 ms. Question 8.
- The backend keeps `order` as a field on the surface, as plan 2.1 says ("we sort surfaces
  ourselves"). The sort happens once per frame, over visible surfaces, which is a few dozen items.

### 2.5 Past the limit

**The protected set `P`.** These are never dropped to make room, because the user can't work
without them, and because a dropped surface must also stop taking input (the test below):

- the cursor, the HUD strip, the taskbar frame and its card, Plasma's bar, the key strip, an Extra
  window and its card, and the panel being grabbed or hovered, with its card.

From the table in 2.2 that's at most 11 layers (cursor 1, HUD strip 1, taskbar 2, Plasma 1, keys 1,
Extra 2, grabbed or hovered panel 2). Folded (a card drawn into its panel's or window's swapchain,
2.3) it's at most 7. HUD marks, other panels, their cards, backs and the theater backdrop are not
protected.

**Budget.** At session start read `maxLayerCount` (call it `M`). The working budget is `B`, starting
at `M`. The floor is `B_MIN = 7`, the size of the folded protected set. `M` is at least 16 on a
conforming runtime (`XR_MIN_COMPOSITION_LAYERS_SUPPORTED`, 2.1), so `B_MIN` is always inside it.

Per frame, build the visible list, sort it by order, and if it's longer than `B`, shed in this
order until it fits:

1. **Out of view.** Drop non-protected surfaces entirely outside the view frustum (the headset's
   `xrLocateViews` FOV with a margin). Cheap, and invisible to the user. This isn't a budget
   measure in the plan; I'm adding it because it cuts the count for free. Question 9.
2. **Backs**, farthest first. The existing `BACKS` cap already does this by count; here it's by
   what fits.
3. **Fold cards into pictures** (2.3), farthest from the head first, protected ones last.
4. **Drop non-protected surfaces.** Lowest priority first: theater backdrop (when other panels
   exist), HUD marks, panels the user isn't looking at (gaze or head-forward angle), then other
   panels, farthest first.
5. **If `P` itself is still longer than `B`** (only possible when `B < 11`, i.e. after a fold and
   after halving, see below): fold every protected card, then drop from `P` in this order, stopping
   as soon as it fits: key strip, HUD strip, Plasma's bar, the Extra window, the taskbar. The cursor
   and the grabbed or hovered panel (folded) are the last two, so at `B = 2` they remain, and at
   `B = 1` the cursor does. A grab in progress ends if the panel can't be kept, with the same
   endings as a destroyed surface (plan section 3, `Pointing`: Up, then Leave).
6. **Never submit zero layers while anything is visible.** Because `P` always contains the cursor
   (or the grabbed panel), the submitted set has at least one layer whenever something is on
   screen. That also covers the Monado quirk that WayVR works around (its comment in
   `wayvr/src/backend/openxr/mod.rs`: "Monado freaks out if no layers are submitted"; **unverified**
   beyond that comment). If everything is hidden, including the cursor, submit one near-transparent
   1x1 quad as WayVR does (`alpha = 0.02` on a 0.001-scale transform), and mark it internal so it
   takes no input.

The brief for this draft said "merge the lowest-priority ones". Merging only works for card plus
picture (step 3). Two unrelated panels have different poses and can't share one quad, so the honest
meaning of "past the limit" is merge what's coplanar, then drop. If you'd rather never drop a
panel, the alternative is a shared composited "wall" for far panels, which I don't think is worth
building (Question 10).

**A dropped surface takes no input.** The hit test (`vr/pointing.rs`, plan section 3) walks only the
list of layers submitted this frame, nearest the top of the array first. A visible surface that
didn't get a layer isn't hittable, hovered or captured. Folding doesn't change this: a folded
card keeps its own rectangle in the hit test and its events are translated to its own surface.

**Test.** A property test over random surface sets (up to 60 surfaces, random kinds, order, one
random grabbed or hovered panel) and `M` in 1, 2, 4, 7, 11, 16 and 64, with the real
`shed` function and no OpenXR. After every shed it checks:

- (a) the submitted count is at most `max(B, 1)`;
- (b) at least one layer is submitted if any surface is visible;
- (c) whenever `B >= 11`, every member of `P` that is visible is drawn in the frame (its own
  layer, or folded into its panel's with its own rectangle in the hit test, as above), so the
  cursor, controls and the grabbed or hovered panel can't vanish while still interactive.
  "Submitted" in (d) means the same;
- (d) whenever `B` is below that, members leave `P` only in the step-5 order, and a surface that is
  not submitted returns nothing from the hit test (so nothing invisible takes a click);
- (e) the same input and `M` give the same list (stable, so nothing flickers between frames).

A second test drives `Pointing` with a hit on a dropped panel and expects no event, and with a
drop during a captured drag and expects Up then Leave.

**Logging.** Once, when the budget first binds, and again only when the shed set changes: `"layers:
runtime allows <M>, budget <B>, <n> surfaces visible; folded <k> cards, dropped <names>"`. Never per
frame. Also log `M` once at start, and put `maxLayerCount`, `B` and the current count in the
`surfaces` control dump.

**If `xrEndFrame` returns `XR_ERROR_LAYER_LIMIT_EXCEEDED` anyway.** This is bounded and has no
loop:

- It means we submitted more than the runtime would take. It does not prove the limit is shared
  with the main session. The man page says the error is for `layerCount` over the limit, and that
  `xrEndFrame` may also fail when the runtime "cannot composite the layers due to resource
  constraints". Shared or not, we can't tell from the error, and the handling doesn't depend on
  which it is.
- On a non-conforming runtime that reports `M < B_MIN`, the floor is `M` itself, and a
  layer-limit error goes straight to the step-5 order below, never raising `B` above `M` (Muse,
  #385). The property test's `M` of 1, 2 and 4 covers that branch.
- On the error: `B = max(B_MIN, B / 2)`, applied from the next frame. We don't resubmit in the same
  frame (whether a failed `xrEndFrame` leaves the frame open is **unverified**), so we lose one
  frame's layers. Log once per halving.
- The halving stops at `B_MIN`, so at most two halvings happen (`16 -> 8 -> 7`; a larger `M` takes
  a few more). `B` never grows back during a session, so there's no oscillation.
- If the error comes back at `B_MIN`, step down to the step-5 order with a hard cap of 2 layers
  (cursor and the grabbed panel). If that fails too, stop submitting layers, log once, and retry
  an empty-but-keepalive frame once a second with no tight loop. The session isn't ended.
- A different error from `xrEndFrame` (anything other than the layer limit) is not this path.

## 3. The interim room rule

### 3.1 What the code does today

- `Prop_CurrentUniverseId_Uint64` is read at start (`main.rs:972`, `poll::Live.universe_id()`)
  and on a relocalization (`main.rs:540-550`). `config::UNIVERSE` holds `conf::room(universe)` and
  `windows.rs` (`same_room`) and the Workspace window read it.
- `conf::room(universe)` maps 424242 and 525252 (the Frame's dummy universes) to 0, and 0 means
  "no room" (`crates/cc-proto/src/conf.rs:1030`).
- `config::choose_workspace` logs "SteamVR has no room yet" when `room(universe) == 0`, then
  asks `conf::choose_workspace` (`config.rs:237-252`).
- `conf::choose_workspace(data, universe, hint)` (`conf.rs:1106`):
  - `here(n)` is false for every workspace when `universe == 0` (it needs `universe != 0`), so
    rule 1 (a dedicated workspace bound to this room) never matches.
  - Rule 2 (`universe == 0`): the hint's workspace if it exists, else the active workspace if it
    exists, else Temporary (created if missing, `made = true`).
  - Rule 3 (a new room while the active workspace is dedicated and bound to none) needs a
    non-zero room, so it never fires.
  - The last step, `if name == TEMPORARY && universe != 0 { temporary_in(...) }`, is skipped, so
    **Temporary isn't laid out** on room 0: no per-room `rooms` map, no `laid_out_in`, no `seq`,
    just its single `spots`.
- `conf::save_workspace_as` only sets the new workspace's `universe` field if `room(universe) != 0`,
  so "Save as workspace" on OpenXR would make a workspace bound to nothing, and the next start
  sees it as unbound.
- `windows.rs`'s `same_room` treats a kept window pose from room 0 as "can't tell, so as before"
  (its test `a_kept_window_pose_only_comes_back_in_its_own_room`, last assert), so poses written
  with room 0 come back, but they also come back in any room.

The plan's phrase "universe 0 maps to room 0, so Temporary isn't laid out" is right, and the
consequences on OpenXR with no change are:

- Spots **do** save and restore, into whichever workspace is active (the hint's, or the one
  `home.json` last had, or Temporary on a fresh install). That already works, because spots live
  in the workspace, not in the room.
- But: no per-room layouts for Temporary. A "dedicated" workspace can't be bound to a room, so
  `workspace new` and Save as workspace can't pin it. The Workspace window's room list
  (`rows_of(&d, UNIVERSE, ..)`) has nothing to show. The workspace hint becomes the only switch
  (rule 2). Window poses (`windows.rs`) can't tell rooms apart, which is right for a single room
  but undefined if we ever have two.

### 3.2 The Frame lesson

On the Frame, SteamVR mints a new random universe id each time tracking comes back without
relocalizing against its saved map ("Switching to a new universe ... without a universe
relocalization" in vrserver.txt; `docs/steamvr-integration.md`, "Where things come back after a
restart"; `conf.rs` doc comment on `LAID_OUT_IN`). That's why Temporary starts a new room from the
last room's layout and keeps at most `KEEP_ROOMS = 8`. The lesson for OpenXR: **a runtime-given
room id would have the same problem or the opposite one**. We can't assume an id is stable, and
we can't assume it changes when the room does.

### 3.3 The rule

**One default room per runtime, named in the persisted identity by its backend. A room is
`<backend>:<key>`. `Session::universe_id()` becomes a `RoomId`. On OpenXR it's
`openxr:<runtime>`, fixed for the run. A workspace is what separates layouts.**

This replaces my first idea (a hashed `u64` with a tag bit in the top bits). A tag bit is a
probabilistic guarantee that SteamVR never uses that bit (**unverified**), and a hash is unreadable
in `home.json`. A namespace in the identity is exact, and it shows up in the file.

- **Why the id is already a string.** Rooms are persisted as strings: a workspace's `"universe"` and
  `"universes"` (`conf::rooms`, `conf.rs:972`, which also accepts a hand-edited number),
  Temporary's `"laid_out_in"` and the keys of its `"rooms"` map (`temporary_in`, `conf.rs:1057`).
  Only the in-memory side is a `u64`: `conf::room(universe)`, `conf::choose_workspace(data, universe,
  ..)`, `conf::add_room`, `conf::save_workspace_as`, `config::UNIVERSE` (an `AtomicU64`) and
  `windows.rs`'s kept pose, which writes `"room"` as a JSON number (`windows.rs:1184`).
- **The format.**
  - OpenVR rooms stay exactly as they are: the bare decimal universe, e.g. `"5873160041"`. A string
    with no `:` is read as `openvr:<that>`. We keep writing them bare, so no stored value changes
    and an older build keeps reading the file.
  - OpenXR rooms are written prefixed: `"openxr:monado"`, `"openxr:wivrn"`.
  - The key is a small readable name for known runtimes, matched from `runtimeName` in
    `xrGetInstanceProperties`: contains "monado" gives `monado`, contains "wivrn" gives `wivrn`.
    Anything else gets `x-<8 hex digits of a hash of runtimeName>`, so two unknown runtimes still
    don't share a room, and a human can see it's a hash. **Unverified:** what `runtimeName` string
    Monado and WiVRn actually report (WiVRn is built on Monado, so it may say "Monado"). The
    runtime manifest's `name` field (`active_runtime.json`, which `tools/xr-probe` already reads)
    is the fallback identity if the string isn't distinguishing.
  - The key identifies the runtime, nothing about the headset or the room. Question 11 (no
    `systemName`) is settled that way.
- **Per runtime, not per backend:** SteamVR's standing space and Monado's stage space have
  unrelated origins, so they must not select each other's spots. The namespace guarantees that
  `openvr:123` and `openxr:monado` are never equal, which a numeric id could only promise by luck.
- **Per workspace** needs no new code. A workspace already holds its own `spots`. A dedicated
  workspace can be bound to an OpenXR room exactly as it can be bound to a universe today. Its
  `universes` list may hold both kinds, if the user binds one workspace to a SteamVR room and a
  Monado room on purpose. That's the "per runtime and workspace" key: `(room id, workspace) ->
  spots`.
- **What it buys:** `conf::choose_workspace` runs its normal rules. Rule 1 finds a workspace bound
  to the room. Rule 3 binds a new dedicated workspace to it. `save_workspace_as` binds Save as
  workspace to it. If the user has no dedicated workspace, Temporary is laid out in the room
  (`temporary_in`) with one entry, and since the room never changes, it never swaps.
- **No logging noise.** `config::choose_workspace`'s "SteamVR has no room yet" line is for an empty
  room, which doesn't happen on OpenXR any more. The message says "SteamVR", so it should say "the
  runtime".

What has to change (no code written, this is the list):

1. **A `RoomId` type in `crates/cc-proto/src/conf.rs`,** next to `room()`: a backend (`OpenVr`,
   `OpenXr`) and a key, with `Display` and `FromStr` as above (no `:` means OpenVR), and `None`
   for "no room yet". `room(universe: u64)` stays and builds an OpenVR `RoomId`, still zeroing the
   Frame's two dummy ids (424242, 525252).
2. **`Session::universe_id()` returns `Option<RoomId>`** (the plan's section 3 has `Option<u64>`).
   OpenVR returns what SteamVR says, through `room()`. OpenXR returns `openxr:<runtime>`.
3. **Plumb the string.** `conf::choose_workspace`, `add_room`, `save_workspace_as` and
   `temporary_in` take `&RoomId` (or its string) where they take a `u64` now. `rooms()` already
   returns strings, so the comparison `rooms(ws).contains(&id)` is unchanged. `config::UNIVERSE`
   becomes a small `Mutex<Option<String>>` (or the string behind a cheap clone). `control.rs`'s
   `universe` command prints it.
4. **Window poses.** `windows.rs` writes `"room"` as a number today. For an OpenXR room it writes
   the string. `same_room` reads a number as `openvr:<n>`, a string as itself, and a missing value
   as "an older file's: as before". Old files keep working, and the existing test
   (`a_kept_window_pose_only_comes_back_in_its_own_room`) gets one more case.
5. **Relocalization.** `S::Relocalized` and `S::RoomChanged` come from `Session::next_event`. On
   OpenXR, `XrEventDataReferenceSpaceChangePending` on the STAGE space gives `Relocalized` (plan
   2.7). It never gives `RoomChanged`, since the id doesn't change. So the room is permanent for
   the run.
6. **Migration of `home.json`: none required.** Existing values are bare decimals, which read as
   `openvr:<n>`, and OpenXR values are new keys added next to them. Nothing is rewritten on load.
   Compatibility with older builds: they read ids as opaque strings (`conf::rooms`'s `id` closure
   takes any non-empty string), so an OpenXR id in `universes` simply never matches a SteamVR
   universe there. An older build could, in principle, set `"universe"` to its first room from
   such a list; it wouldn't match anything, which is the same as an unbound workspace. I haven't
   tested an older build against such a file (**unverified**). If we later want to normalise
   everything to `openvr:<n>`, that's its own step with a one-way write, and I'd not do it in phase 1.
7. **Tests:** add a fixture where the room is `openxr:monado` and check (a) a workspace bound to
   it is chosen, (b) Temporary lays out in it once and doesn't swap, (c) Save as workspace binds
   it, (d) `openvr:<n>` and `openxr:monado` never select the same dedicated workspace, even if a
   hand-edited `universes` list holds a bare `"0"` or a number; (e) bare decimals still parse as
   OpenVR and round-trip unchanged through a load and save. The `old_loop` golden in `main.rs`
   tests is OpenVR-only and doesn't change.
8. **Docs:** `docs/steamvr-integration.md`'s "Where things come back after a restart" and
   `docs/workspaces.md` get an OpenXR paragraph (every user-facing change gets its docs in the same batch), and the
   home.json format description (`conf.rs` comment near 818, `docs/workspaces.md`) gets the room id
   format.

### 3.4 What this doesn't solve, and the risk

- **The origin may not be the same room twice.** On SteamVR the Frame's "places may be off until
  you move them" after a restart, because the origin is wherever the headset started
  (`steamvr-integration.md`). I expect Monado and WiVRn to behave like that, or worse (Monado's
  stage origin depends on its tracking driver and calibration; **unverified**). With one default
  room per runtime, spots from yesterday are restored relative to today's origin. So "a relaunch
  puts the panels back where the user left them" (the plan's Phase 1 "done when") is true in
  space only if the runtime keeps its origin. Phase 1 should say so in its test, and the cheap
  fix is a user action: a "recenter" that moves all panels by the difference between the old
  and new head position, the same job `grab.relocalized` and the reanchor path do for SteamVR
  (the code is named in `main.rs:540-545`; I haven't read its body).
- **Two rooms on one runtime** (a Quest user who walks between flats) get one layout, and the user
  makes one workspace per room. This is the plan's "workspaces the user names", moved up to
  phase 1 only in that it works by hand.
- **Later options** are in the plan: a persisted spatial anchor (`XR_MSFT_spatial_anchor`,
  `XR_FB_spatial_entity`, unverified support), or a runtime map id (Monado SLAM, unverified).
  When one exists, `universe_id()` returns it instead, with nothing else changing, which is why
  the rule goes through `Session` and not around it.

## 4. WayVR, what it tells us and what it doesn't

All paths in https://github.com/wlx-team/wayvr at `ef7e5f6`.

- **Overlay session:** `wayvr/src/backend/openxr/helpers.rs`: requires `XR_EXTX_overlay`
  (`"Missing EXTX_overlay extension."`), chains `SessionCreateInfoOverlayEXTX` (flags empty,
  `session_layers_placement: 5`) into the `GraphicsBindingVulkanKHR` for `xrCreateSession`.
  `mod.rs` handles `MainSessionVisibilityChangedEXTX` and draws a skybox only when the main
  session isn't visible.
- **Layers:** `wayvr/src/backend/openxr/overlay.rs` makes quad or cylinder layers in the STAGE
  space, `BLEND_TEXTURE_SOURCE_ALPHA`, optional colour scale and bias for alpha, optional
  `CompositionLayerAlphaBlendFB`. One swapchain per overlay, one or two layers per overlay (stereo
  uses an array of two and `EyeVisibility` per eye). It sorts all layers by distance each frame
  (`mod.rs`, "Layer composition"). It does no `maxLayerCount` check.
- **Swapchains:** `swapchain.rs`: created with `COLOR_ATTACHMENT | SAMPLED`, images wrapped as
  external, acquire and wait at render, release at present, and a retirement queue gated on a GPU
  completion marker when replaced.
- **Frame loop:** it submits an almost-invisible layer when everything is hidden ("Monado freaks
  out if no layers are submitted", a comment in `mod.rs`). Relevant to us: **don't submit zero
  layers on Monado** (**unverified** beyond that comment). Our cursor or HUD may often cover it,
  but a state with nothing visible needs the same trick.
- **dmabuf import:** `wayvr/src/graphics/dmabuf.rs` (`dmabuf_texture`, `get_drm_formats`,
  `fourcc_to_vk`) over `wgui/src/gfx/raw.rs` (`import_dmabuf`): dedicated allocation, dup'd fd,
  explicit modifier and plane layouts when the modifier is known, `OPTIMAL` tiling otherwise,
  queue-family-foreign barriers. It imports a new `VkImage` for each received frame
  (`overlays/screen/capture.rs`, `receive_callback`) with no cache I could see, which is simpler
  than ours but allocates per frame. We cache.
- **What it lacks that we need:** explicit sync (see 1.6), a cache, and a fallback when import
  fails other than dropping the frame (it logs `Failed to create DMA-buf vkImage` and returns
  `None`; separately, memfd frames are uploaded, `overlays/screen/capture.rs`). A layer budget.
  Draws rather than copies.
- The plan's description of WayVR matches what I found (overlay required, no foreground fallback,
  Monado IPC for the rest).

## 5. Open questions

Numbered so you can answer by number. The first three block phase 1. Where Muse (board topic 129,
#377) proposed an answer, it's recorded as **Proposed (Muse)**. These are proposals, and Anthony decides.

1. **Copy or draw?** Is a format-matched `vkCmdCopyImage` enough for v1 (fewer moving parts, needs
   a swapchain format that matches the byte order), or do we build the draw pass now because the
   quarter-turned vertical panels and crops will want it anyway? My lean: copy, and build the draw
   pass only if the spike shows a runtime without a matching format.
   **Proposed (Muse):** copy first, no draw pass unless the spike finds a runtime that needs it.
2. **Does a back need to be mirrored?** `back.rs` mirrors it and dims it. On OpenXR a shared
   swapchain can't mirror, and can't dim without colour scale and bias. Is an un-mirrored, dimmed
   (or undimmed) back acceptable, or should a back get its own (cheap, plain-sheet) swapchain? The
   rule the module opens with is "the freedom to place them backwards, but still have an idea that
   a panel is there".
   **Proposed (Muse):** no mirroring in phase 1. Dim through the `FILL` sheet if that's cheap; otherwise the
   back is un-mirrored and undimmed, and that is documented.
3. **Which modifiers do we offer PipeWire?** Today one (`capture.rs` `format`). Should it offer the
   set `vkGetPhysicalDeviceImageFormatProperties2` says the device can import, as WayVR does
   (`graphics/dmabuf.rs` `get_drm_formats`)? That changes the SteamVR path too if shared. Offer it
   only on the OpenXR backend?
   **Proposed (Muse):** offer the importable modifier set, on the OpenXR backend only.
4. **sRGB or UNORM** for swapchains, per runtime? **Proposed (Grok, #380):** `_SRGB`, since our
   bytes are sRGB-encoded and a copy doesn't convert them (1.5). The spike confirms it.
5. **Is fence option A (POLLIN) good enough,** or do we go straight to `sync_file`? A is what we do
   now and costs nothing; B is right on principle. Does anyone know whether KWin's screencast
   attaches fences reliably? If you know SPA explicit sync is available in the PipeWire we build
   against, B gets cheaper.
6. **Should the loop ever park on OpenXR?** The plan says no while a session runs. If power
   measurement says we must, how does a fence completion wake it?
7. **`sessionLayersPlacement`.** What value? 0 (always first among overlays), a high value, or
   a setting? WayVR uses 5 without saying why. Overlays from different apps stack by this number
   (13.1.2) and a newer one with the same value is nearer the user. Our overlay should probably be
   above WayVR-type apps' (so the cursor isn't hidden), or below, depending on who we expect to
   share a Monado with. **Unverified**: how Monado's compositor implements it.
8. **Paint order source.** Keep `paint_order`'s ranks (view-overlap sort, about 4 times a second),
   or add WayVR's per-frame distance sort for tie-breaking during head motion? Mine is more
   accurate for wide panels, theirs is cheaper to reason about and faster to react.
   **Proposed (Muse):** keep `paint_order`.
9. **Frustum culling** as the first shed step: do we want it for the budget, or only when the
   budget binds? It could also save copies (don't update a panel nobody sees), which is a separate
   feature and a bigger one.
10. **Is "drop the farthest panel" acceptable,** or should we prefer a hard cap on panels
    (`windows::SLOTS` = 16 is more than 16 layers) and refuse a new window with a message? The
    quiet drop is graceful, but a panel that's "open" and doesn't show is a bug report.
    **Proposed (Muse):** drop quietly and log. Section 2.5 follows this, with the protected set
    and a test that a dropped panel takes no input.
11. **One room per runtime could be too coarse** if the same Linux PC drives both a Monado desktop
    session and WiVRn with a headset. **Proposed (Muse):** no `systemName`. The room is keyed by
    runtime only (section 3.3), and the user separates anything finer with workspaces.
12. **Hash or tag, or constants?** **Proposed (Muse):** small readable constants for known runtimes,
    a hash only for unknown ones. **Superseded in this revision** by the explicit backend namespace
    in 3.3 (`openxr:monado`, `x-<hash>` for unknown runtimes, bare decimals for OpenVR), which keeps
    Muse's constants-plus-hash-fallback idea and drops the tag bit. Still open: confirm the
    namespace format and the `RoomId` plumbing, and whether to normalise OpenVR's ids to
    `openvr:<n>` later (3.3, item 6).

## 6. What I didn't check

- No Monado or WiVRn behaviour was tested. Everything about them above is from the plan, from
  WayVR's source, or marked unverified.
- I did not read `grab.rs` beyond `paint_order` and the order calls, `back.rs` beyond the
  parts quoted, or `rdp.rs`/`vnc.rs` write paths, so the staging fallback for RDP/VNC (1.10) is a
  sketch against `gpu.rs::write_frame`, not a read of the RDP thread.
- The OpenXR man pages for `XR_EXTX_overlay`, `xrReleaseSwapchainImage` and `XR_KHR_vulkan_enable2`
  are stubs; the quotes come from the single-page spec. The spec's text on whether a release
  needs the GPU work finished, not only submitted, I did not find.
- The count in 2.2 depends on the number of configured viewers, which I didn't look up.
- `DMA_BUF_IOCTL_EXPORT_SYNC_FILE` availability by kernel version (6.0) and the semaphore import
  details are from my own knowledge, not from a source I read today.
- WayVR was read from a shallow clone at `ef7e5f6`, outside this repo.
- The Mesa check in 1.6 was a sparse checkout of `src/gbm` and `src/gallium/frontends/dri` at
  `b27d00d`. I did not read the per-driver (radeonsi, iris, freedreno) transfer-map code, so
  whether a driver's CPU map is coherent for a later device read is unverified.
- The retry counts, timeouts and `B_MIN = 7` in 1.7 and 2.5 are proposals I haven't measured.
