# Renderer fonts

These fonts are loaded only by Miyu's long-image renderer worker. The worker
starts with the CJK font and adds the Emoji font only for content that needs it.
They are installed under `/usr/share/miyu/fonts` so rendering is deterministic
and never needs to scan host fonts. Development builds read this directory in
the source tree. `MIYU_RENDERER_FONTS_DIR` can override the location for
portable builds.

- `NotoSansCJK-Regular.ttc`: unmodified Noto Sans CJK release `Sans2.004`
  (`b76b0433203017ca80401b2ee0dd69350349871c4b19d504c34dbdd80541690a`)
- `NotoColorEmoji.ttf`: unmodified Noto Color Emoji version `2.051`
  (`72a635cb3d2f3524c51620cdde406b217204e8a6a06c6a096ff8ed4b5fd6e27b`)

Both fonts are distributed under the SIL Open Font License 1.1. The complete
license texts are stored beside the corresponding font files.
