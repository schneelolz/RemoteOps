# Agent UI design QA

final result: passed

Scope: local native UI implementation and demo interactions on macOS. Windows release acceptance remains pending; this is not a Windows certification.

## Evidence

- Approved source: `docs/design/agent-ui-refresh/design-target.png` (1448×1086), final centered English dark concept.
- Implementation: `docs/design/agent-ui-refresh/en-dark.png` (1600×1200), captured from native Rust/egui demo using Computer Use.
- Also inspected and saved: `zh-light.png`, `zh-dark.png`, `en-light.png`, `logs-dark.png`, `stop-confirmation-dark.png` in that directory.
- Source and actual English dark images were inspected together. Both depict an engineer-connected demo with code 482-915-307; countdown is live rather than fixed at 09:58.
- Compare app-owned content at proportional dimensions; exclude the host's native title bar, capture overlay/pointer and differing OS window controls. Capture is at 2× density; screenshots record the initial 600×450 layout; current configured native content is 500×375 logical points, and its bounds are separately tested without a display.

## Findings and resolution

- Typography: replaced egui strong-only styling with native bold display fonts; CJK fallback installed separately. Code is dominant, supporting labels are smaller, English footer text fits. No unreadable or clipped primary text in the four captured states.
- Layout: number plus copy button centered as one unit; label/countdown centered independently. Capability widths measured including separator spacing to center both languages. Footer actions remain at bottom with clear separation. Fixed-size native layout boundary test passes for both locales and themes.
- Colors: shared semantic palette across main screen and overlays. Light menu selection now uses white text on blue. Filled destructive confirmation uses a darker red for white-label contrast. Manual theme overrides and system preference are tested.
- Assets: existing brand icon for native window; existing Phosphor library for UI icons. No rasterized controls, placeholder art or stretched source screenshot.
- Copy/content: bilingual settings, native title, main labels, tooltips and dialogs. Operational log messages remain raw data, including Chinese demo messages under English UI. Connection heading also represents starting, reconnecting and failure states rather than always claiming ready.
- Interaction: copy success checkmark observed; language and theme changes live; persisted preferences recovered on restart; details opened/closed; logs opened and dismissed using Escape; stop cancellation retains UI, confirmation closes the native window (runtime log records native_window_closed).

No unresolved P0/P1/P2 local findings. P3 differences: settings gear has a visible button affordance; exact system font metrics and icon shapes differ slightly from the generated mock. The native window frame is intentionally OS-owned.

## Validation limits

24 GUI tests + 3 translation tests pass; Clippy with warnings denied and local build pass. Actual Windows compilation, Windows font rendering, 100/125/150% DPI, OS theme change events, and live remote-session behavior need Windows acceptance. See `docs/design/agent-ui-refresh/README.md` for steps.

## Final refinements

Native content is now 500×375. Logs use a full page with Back/Escape navigation and compact metadata; saved original screenshots predate this change. The loading indicator uses three gently fading dots. A permission subtitle is demo-only and does not report or alter live authorization. All workspace check, Clippy and test commands passed on macOS.
