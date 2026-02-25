# actual CLI — Promotional Video

Remotion project for building studio-quality promotional clips for the `actual` CLI.

See [PLAN.md](../PLAN.md) for the full vision, scene breakdown, and implementation roadmap.

## Prerequisites

- Node.js 18+
- `cd promo && npm install`

## Development

```sh
npm run dev   # Opens Remotion Studio at localhost:3000
```

## Rendering

All commands run from the `promo/` directory.

### Hero clip (30s, 16:9, 1920×1080)

```sh
npx remotion render HeroClip out/hero-1080p.mp4 --codec h264 --crf 18
```

### Short clip (15s)

```sh
# 16:9 — Twitter/LinkedIn
npx remotion render ShortClip-169 out/short-169.mp4 --codec h264 --crf 18
# 1:1 — Twitter/LinkedIn square
npx remotion render ShortClip-11 out/short-11.mp4 --codec h264 --crf 18
# 9:16 — Instagram Reels / TikTok
npx remotion render ShortClip-916 out/short-916.mp4 --codec h264 --crf 18
```

### README loop (10s, looping WebM + GIF)

```sh
# WebM
npx remotion render LoopClip out/loop.webm --codec vp8
# GIF (requires ffmpeg): render as PNG sequence first, then convert
npx remotion render LoopClip out/frames/ --sequence
ffmpeg -i out/frames/%04d.png -vf 'fps=20,scale=800:-1:flags=lanczos,palettegen' out/palette.png
ffmpeg -i out/frames/%04d.png -i out/palette.png \
  -vf 'fps=20,scale=800:-1:flags=lanczos,paletteuse=dither=bayer:bayer_scale=5' \
  out/loop.gif
```

## Updating

When the CLI's TUI changes:
1. Update `src/data/tui-states.ts` — adjust step labels, timing, output lines
2. Update `src/branding/banner.rs` art mirror in `src/components/Terminal/LogoPanel.tsx`
3. Re-render affected compositions
