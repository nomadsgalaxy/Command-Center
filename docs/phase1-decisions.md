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
   frame, on the main thread, we acquire the swapchain image, copy into it, submit, and release.
   The source buffer goes back to its producer when the copy's `VkFence` signals, not a tick later.
   PipeWire's implicit fence (`POLLIN`) stays as the first gate, and a `sync_file` exported from the
   dmabuf and waited on by a semaphore is the target for the GPU side. A failed import falls back
   to a staging upload and says so once. A panel's back is a second layer on the front's swapchain,
   so nothing is imported or copied twice.
2. **Layer budget.** Read `maxLayerCount` once at session start. Normally one layer per visible
   surface, sorted ascending by `Surfaces::order`. Over budget: drop backs first, then fold each
   panel's card into its picture, then drop the lowest-priority surfaces, with one log line naming
   what went.
3. **Room rule.** `Session::universe_id()` on OpenXR returns one fixed, non-zero id per runtime. A
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

Also unresolved: sRGB versus UNORM swapchain formats and how runtimes treat them for a quad. The
spec has per-format colour-space rules I haven't pinned down for this case. The spike should look
at one known-colour test card on each runtime (Question 4).

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
  `gbm_bo_unmap`, so there is no producer fence. Strictly, a CPU write to a dmabuf wants
  `DMA_BUF_IOCTL_SYNC` start and end around it so the cache is coherent for a device read. On the
  drivers we use (linear, write-combined maps) it appears to be fine today because SteamVR reads
  them the same way, but I haven't checked this for Turnip or others (**unverified**). The spike
  tests it.
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
- *Before `xrReleaseSwapchainImage`.* The copy has to be **submitted**. Whether it also has to be
  **finished** is the open point: the runtime shares our queue and the spec says only that the
  image must be in the stated layout and owned by the queue on release. Queue submission order
  gives an execution order on one queue, so I expect that submit-then-release is enough, but I
  haven't found the sentence that says so (**unverified**; the spec lines I found are about layout
  and ownership). Baseline: wait on the fence before releasing. Then try submit-only in the spike
  with a runtime-side validation pass.

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

### 1.7 Release

- `Textures::release(t)` marks an import for destruction after its last fence. The sweep runs
  once per frame, on the main thread, with `vkGetFenceStatus`, never `vkWaitForFences` in the frame
  path.
- Teardown (session ending, `STOPPING` or `EXITING`, or `InstanceLossPending`): release any acquired
  image, `vkQueueWaitIdle`, destroy imports, then `xrDestroySwapchain`, then the device. The
  retirement queue pattern above covers a single swapchain being replaced mid-session.
- A resize frees nothing at once. The old swapchain and the old pair of GBM buffers both wait for
  their fences. This fixes the case the plan says is unsafe on OpenVR today ("A panel's resize
  frees both buffers at once, the one on screen included").

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
   the instant their fence signals, and a checksum of what's displayed matches the pre-rewrite
   frame (this is the test that "a tick later" isn't needed).
4. **Negative control.** Rewrite a source before its fence signals on purpose and confirm the test
   sees corruption, so we know the check works.
5. **Resize.** 100 random resizes of the swapchain and the source, no leak and no use-after-free.
6. **Reconnect.** Drop the PipeWire stream and reopen it 100 times (buffers are renegotiated, the
   `BufId` generation changes). A stale give-back never reaches a new buffer.
7. **Teardown.** End the session mid-frame (acquired, not released) and exit. Count open fds in
   `/proc/self/fd` and device memory (`VK_EXT_memory_budget`) before and after: unchanged.
8. **Failure path.** Force an import failure (an unsupported modifier) and confirm the staging
   fallback shows the same picture, with one log line.
9. **Where.** The Vulkan half (items 2 to 8 without the XR calls, copying into a plain
   `VkImage`) runs on the Frame's Turnip stack, since the Frame can't run Monado. The XR half runs on a
   PC with Monado's simulated HMD, and WiVRn with a Quest if one's available (plan 4, Phase 1).
   Targets: AMD (radv) and Intel (anv) on x86, Turnip on aarch64, and whatever the Quest-host PC
   has. I don't know which Mesa versions the Frame's SteamOS ships for Vulkan dmabuf import
   (**unverified**).

Pass means all of 2 to 8 on each target. A target that fails item 3 on A-style fencing moves to
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

At session start read `maxLayerCount` (call it `M`). Per frame, build the visible list, sort it by
order, and if its length exceeds `B = M - R` (reserve `R` for the cursor and, if we find the budget
is shared with the main session, whatever the game uses, **unverified**; start with `R = 0`), shed
in this order until it fits:

1. **Out of view.** Drop surfaces entirely outside the view frustum (the headset's `xrLocateViews`
   FOV with a margin, and not being grabbed). Cheap, and invisible to the user. This isn't a
   budget measure in the plan; I'm adding it because it cuts the count for free. Question 9.
2. **Backs**, farthest first. The existing `BACKS` cap already does this by count; here it's by
   what fits.
3. **Fold cards into pictures** (2.3), farthest from the head first.
4. **Drop the lowest-priority surfaces.** Priority, lowest first: theater backdrop (when other
   panels exist), panels the user isn't looking at (gaze or head-forward angle), then other
   panels, farthest first. Never dropped: cursor, HUD, taskbar, Plasma's bar, an Extra window,
   the panel being grabbed or hovered.
5. If it still doesn't fit, `xrEndFrame` would fail, so submit only the top `B` and log.

The brief for this draft said "merge the lowest-priority ones". Merging only works for card plus picture
(step 3). Two unrelated panels have different poses and can't share one quad, so the honest
meaning of "past the limit" is merge what's coplanar, then drop. If you'd rather never drop a
panel, the alternative is a shared composited "wall" for far panels, which I don't think is worth
building (Question 10).

**Logging.** Once, when the budget first binds, and again only when the shed set changes: `"layers:
runtime allows <M>, <n> surfaces visible; folded <k> cards, dropped <names>"`. Never per frame.
Also log `M` once at start, and put `maxLayerCount` and the current count in the `surfaces`
control dump.

**If `xrEndFrame` returns `XR_ERROR_LAYER_LIMIT_EXCEEDED` anyway,** because the real limit is below
`M` (the shared-budget case), halve the budget for the rest of the session, log it, and continue.
That's a cheap safety net for the **unverified** shared-limit case.

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

**One default room per runtime. `Session::universe_id()` on OpenXR returns `Some(id)`, where `id`
is a fixed non-zero number for that runtime. A workspace is what separates layouts.**

- **Per runtime, not per backend.** The id is derived from the runtime's name from
  `xrGetInstanceProperties` (`runtimeName`, e.g. "Monado", "WiVRn"), hashed to a `u64` with a
  fixed tag in the top bits so it can't collide with a SteamVR universe id (which are random
  64-bit values, so this is a probabilistic guarantee; **unverified** that SteamVR never uses the
  top bit). The same layout would otherwise be shared between SteamVR's standing space and Monado's
  stage space, whose origins have nothing to do with each other.
- **Per workspace** needs no new code. A workspace already holds its own `spots`. A dedicated
  workspace can be bound to this id exactly as it can be bound to a universe today. That's the
  "per runtime and workspace" key: `(runtime id, workspace) -> spots`.
- **What it buys:** `conf::choose_workspace` runs its normal rules. Rule 1 finds a workspace bound
  to the default room. Rule 3 binds a new dedicated workspace to it. `save_workspace_as` binds
  Save as workspace to it. If the user has no dedicated workspace, Temporary is laid out in the
  default room (`temporary_in`) with one entry, and since the room never changes, it never swaps.
- **No logging noise.** `config::choose_workspace`'s "SteamVR has no room yet" line is for room 0,
  which doesn't happen on OpenXR any more. The message says "SteamVR", so it needs to say "the
  runtime" if it ever fires there.

What has to change (no code written, this is the list):

1. **`Session::universe_id()` for the OpenXR backend** returns `Some(openxr_default_room(runtime))`.
   OpenVR is unchanged (returns what SteamVR says, including the dummy ids that `room()` zeroes).
2. **`conf::room()`** stays as is. The default room id is non-zero and not one of the two dummy
   values.
3. **A constant and a function for the default room,** in `crates/cc-proto/src/conf.rs` next to
   `room()` so both crates agree and a test can pin it, e.g. `pub fn default_room(runtime: &str) ->
   u64`.
4. **Message text** in `config::choose_workspace` and `main.rs` that says "SteamVR" for the
   universe: say "the runtime" (it reads the room from `Session`).
5. **Relocalization.** `S::Relocalized` and `S::RoomChanged` come from `Session::next_event`. On
   OpenXR, `XrEventDataReferenceSpaceChangePending` on the STAGE space gives `Relocalized` (plan
   2.7). It never gives `RoomChanged`, since there's no id. So the default room is permanent for
   the run.
6. **Test:** add a fixture where `universe_id` is the default room and check (a) a workspace bound
   to it is chosen, (b) Temporary lays out in it once and doesn't swap, (c) Save as workspace binds
   it, (d) a SteamVR id and the default room never select the same dedicated workspace by
   accident. The `old_loop` golden in `main.rs` tests is OpenVR-only and doesn't change.
7. **Docs:** `docs/steamvr-integration.md`'s "Where things come back after a restart" and
   `docs/workspaces.md` get an OpenXR paragraph (the public-release rule in my notes).

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

Numbered so you can answer by number. The first three block phase 1.

1. **Copy or draw?** Is a format-matched `vkCmdCopyImage` enough for v1 (fewer moving parts, needs
   a swapchain format that matches the byte order), or do we build the draw pass now because the
   quarter-turned vertical panels and crops will want it anyway? My lean: copy, and build the draw
   pass only if the spike shows a runtime without a matching format.
2. **Does a back need to be mirrored?** `back.rs` mirrors it and dims it. On OpenXR a shared
   swapchain can't mirror, and can't dim without colour scale and bias. Is an un-mirrored, dimmed
   (or undimmed) back acceptable, or should a back get its own (cheap, plain-sheet) swapchain? The
   rule the module opens with is "the freedom to place them backwards, but still have an idea that
   a panel is there".
3. **Which modifiers do we offer PipeWire?** Today one (`capture.rs` `format`). Should it offer the
   set `vkGetPhysicalDeviceImageFormatProperties2` says the device can import, as WayVR does
   (`graphics/dmabuf.rs` `get_drm_formats`)? That changes the SteamVR path too if shared. Offer it
   only on the OpenXR backend?
4. **sRGB or UNORM** for swapchains, per runtime? I haven't pinned the colour rules for quads and
   the spike will have to show it.
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
9. **Frustum culling** as the first shed step: do we want it for the budget, or only when the
   budget binds? It could also save copies (don't update a panel nobody sees), which is a separate
   feature and a bigger one.
10. **Is "drop the farthest panel" acceptable,** or should we prefer a hard cap on panels
    (`windows::SLOTS` = 16 is more than 16 layers) and refuse a new window with a message? The
    quiet drop is graceful, but a panel that's "open" and doesn't show is a bug report.
11. **One shared default room per runtime** could be too coarse if the same Linux PC drives
    both a Monado desktop session and WiVRn with a headset. Should the id mix in the system name
    (`xrGetSystemProperties.systemName`) as well as the runtime? That separates headsets, not rooms.
12. **Hash or tag:** a hashed `u64` with a tag bit, or a deliberately small constant (say
    `1`..`15`) that no real SteamVR id would plausibly equal? Constants are readable in
    `home.json`; hashes are safer against collision. I lean to small readable constants per known
    runtime name, with a hash for unknown ones.

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
- The WayVR checkout was a shallow clone in `~/.cache/agent-scratch/cc/wayvr/` (not part of the
  repo), left in place for the reviewers to grep. Delete it when done.
