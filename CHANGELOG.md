# Changelog

## Unreleased

- `ui.theme` and `ui.thread_preview_lines` now affect the running interface.
  Values that older releases accepted but left inert now fail startup validation
  when the theme is not `system`, `light`, or `dark`, or when the preview limit
  is outside 1 through 20.
