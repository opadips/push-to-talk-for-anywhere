# Ideas (out of scope for v1)

Everything here is explicitly **not built** in v1 — see IMPLEMENTATION_PLAN.md §1.
Add to this list instead of growing the v1 scope.

macOS and Linux are no longer in the exclusion list: v1 shipped on Windows and
covers Linux (X11) and macOS. Further per-platform polish (Wayland, `.deb` /
`.app` bundles, x86_64 macOS builds) belongs in the plain feature list below.

- Key combinations with modifiers
- Per-application profiles
- Noise suppression
- Mute lock (ignore mute changes made by drivers/apps — `IAudioEndpointVolumeCallback`)
- Raw Input backend for hold-to-talk as well (already used for capture —
  bug 3: while the settings window's WebView2 has keyboard focus, key
  presses never reach the low-level keyboard hook, so holding the binding
  *over the settings window* does nothing and the key is not swallowed
  there either; mouse input and unfocused keyboard input are unaffected)
- Network features or telemetry (v1 is offline-only, plan §11)
- Watchdog helper process, elevated-at-login option (M6 stretch, plan §9)
