import React from "react";
import { Sequence, useCurrentFrame, useVideoConfig, interpolate } from "remotion";
import { TerminalWindow } from "../Terminal/TerminalWindow";
import { TuiLayout } from "../Terminal/TuiLayout";
import { SceneComplete } from "./SceneComplete";
import { SceneCta } from "./SceneCta";
import { FilmGrain } from "../effects/FilmGrain";
import { Vignette } from "../effects/Vignette";
import { COLORS, FONTS } from "../../data/brand";
import { COPY } from "../../data/copy";
import { getStateAtFrame, FRAMES } from "../../data/tui-states";

// Instant hook: terminal already showing command, fades in over 15 frames
const InstantHook: React.FC = () => {
  const frame = useCurrentFrame();
  const opacity = interpolate(frame, [0, 15], [0, 1], {
    extrapolateRight: "clamp",
  });
  return (
    <div
      style={{
        width: "100%",
        height: "100%",
        background: COLORS.background,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        opacity,
      }}
    >
      <TerminalWindow width={1200} height={640}>
        <div
          style={{
            fontFamily: FONTS.mono,
            fontSize: 14,
            color: COLORS.textPrimary,
            padding: "24px 20px",
          }}
        >
          <span style={{ color: COLORS.borderGreen }}>{COPY.shell.prompt}</span>
          <span style={{ color: COLORS.textDim }}> ❯ </span>
          <span>{COPY.shell.command}</span>
        </div>
      </TerminalWindow>
    </div>
  );
};

// Pipeline compressed to 660 frames, mapping the full 960-frame pipeline range
const FastPipeline: React.FC = () => {
  const frame = useCurrentFrame();
  const pipelineDuration = FRAMES.WRITE_END - FRAMES.REVEAL_END; // 960
  const absoluteFrame =
    FRAMES.REVEAL_END + Math.floor((frame * pipelineDuration) / 660);
  const state = getStateAtFrame(absoluteFrame);

  const confirmAbsRelFrame = FRAMES.CONFIRM_APPEAR - FRAMES.REVEAL_END;
  const confirmShortFrame = Math.floor(
    (confirmAbsRelFrame * 660) / pipelineDuration
  );

  const cameraProgress = interpolate(
    frame,
    [confirmShortFrame - 5, confirmShortFrame + 20],
    [0, 1],
    { extrapolateLeft: "clamp", extrapolateRight: "clamp" }
  );
  // No zoom-out in FastPipeline: ConfirmWidget is present in TUI state 7
  // (abs 490–569) all the way to the last frame of FastPipeline (abs 569).
  // There is no post-accept window to zoom back into — the hard cut to
  // SceneComplete resets the camera naturally.
  const cameraScale =
    frame < confirmShortFrame
      ? 1.0
      : interpolate(cameraProgress, [0, 1], [1.0, 1.15]);

  const cameraY =
    frame < confirmShortFrame
      ? 0
      : interpolate(cameraProgress, [0, 1], [0, -40]);

  const completedCount = state.steps.filter(
    (s) => s.status === "success" || s.status === "warning"
  ).length;

  return (
    <div
      style={{
        width: "100%",
        height: "100%",
        background: COLORS.background,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      <div
        style={{
          transform: `scale(${cameraScale}) translateY(${cameraY}px)`,
          transition: "none",
        }}
      >
        <TerminalWindow
          width={1200}
          height={640}
          glowIntensity={(completedCount / 5) * 0.4}
        >
          <TuiLayout
            steps={state.steps}
            activeStepIndex={state.activeStepIndex}
            outputLines={state.outputLines}
            confirmWidget={state.confirmWidget}
            currentFrame={absoluteFrame}
          />
        </TerminalWindow>
      </div>
    </div>
  );
};

// Short clip: 1080 frames (18s at 60fps)
// Hook: 60f, FastPipeline: 660f (full pipeline compressed), Complete: 120f, CTA: 240f
//
// Three layout modes driven by canvas dimensions:
//   Wide (16:9, 1920×1080)  — 1920×1080 container, terminal centred.
//   Square (1:1, 1080×1080) — same 1920×1080 container, terminal shifted left
//                             by SQUARE_TERM_OFFSET to expose the logo+steps panel.
//   Portrait (9:16, 1080×1920) — same 1920×1080 scene block as square (same left
//                             shift), scaled up so the terminal fills the middle 60%
//                             of the canvas height. Top/bottom 384px bands are used
//                             by the CTA exactly like the square layout.

// 15% of 1080px — shifts the 1920-wide content left to expose the logo+steps panel.
const SQUARE_TERM_OFFSET = -162;

// Portrait (9:16) — scale-to-fill.
// Scale 1.8 so the 640px-tall terminal occupies 60% of the 1920px canvas height
// (640 × 1.8 = 1152px), centred vertically → 384px symmetric top/bottom bands.
// The same SQUARE_TERM_OFFSET shift is applied inside the block so the left-side
// TUI panel fills most of the canvas width, matching the ShortClip-11 framing.
const PORTRAIT_SCENE_SCALE = 1.8;
const PORTRAIT_TERM_LEFT   = 20;   // canvas-px gap before terminal left border
// Symmetric vertical centering: (1920 − 640 × 1.8) / 2 = 384
const PORTRAIT_TERM_TOP    = (1920 - 640 * PORTRAIT_SCENE_SCALE) / 2;
// CSS transform on the 1920×1080 scene block.
// canvas_coord = scale × local_coord + (tx, ty)
// Terminal local position after SQUARE_TERM_OFFSET shift: x=198, y=220
//   tx = PORTRAIT_TERM_LEFT  − scale × 198  =  20 − 356.4 ≈ −336
//   ty = PORTRAIT_TERM_TOP   − scale × 220  = 384 − 396   = −12
const PORTRAIT_BLOCK_TX = Math.round(
  PORTRAIT_TERM_LEFT - PORTRAIT_SCENE_SCALE * ((1920 - 1200) / 2 + SQUARE_TERM_OFFSET)
);
const PORTRAIT_BLOCK_TY = Math.round(
  PORTRAIT_TERM_TOP  - PORTRAIT_SCENE_SCALE * ((1080 - 640) / 2)
);

export const ShortClip: React.FC = () => {
  const { width, height } = useVideoConfig();
  const isSquare = width === height;
  const isPortrait = height > width; // 9:16 (1080×1920)

  // ── Portrait (9:16) layout ─────────────────────────────────────────────────
  // The entire 1920×1080 scene block (with SQUARE_TERM_OFFSET shift applied
  // inside) is scaled up via CSS transform so the terminal fills the middle
  // 60% of the 1920px canvas height. Seq1–3 live inside the transformed block;
  // SceneCta renders as an absolute overlay at full canvas size.
  if (isPortrait) {
    const blockStyle: React.CSSProperties = {
      position: "absolute",
      left: 0,
      top: 0,
      width: 1920,
      height: 1080,
      transform: `translateX(${PORTRAIT_BLOCK_TX}px) translateY(${PORTRAIT_BLOCK_TY}px) scale(${PORTRAIT_SCENE_SCALE})`,
      transformOrigin: "top left",
    };
    const shiftStyle: React.CSSProperties = {
      width: "100%",
      height: "100%",
      transform: `translateX(${SQUARE_TERM_OFFSET}px)`,
    };
    return (
      <div style={{ position: "relative", width: 1080, height: 1920, overflow: "hidden" }}>
        {/* Seq 1 – Hook */}
        <Sequence from={0} durationInFrames={60}>
          <div style={blockStyle}>
            <div style={shiftStyle}>
              <InstantHook />
            </div>
          </div>
        </Sequence>
        {/* Seq 2 – Pipeline */}
        <Sequence from={60} durationInFrames={660}>
          <div style={blockStyle}>
            <div style={shiftStyle}>
              <FastPipeline />
            </div>
          </div>
        </Sequence>
        {/* Seq 3 – Complete */}
        <Sequence from={720} durationInFrames={120}>
          <div style={blockStyle}>
            <div style={shiftStyle}>
              {/* FastPipeline ends at 1.15x / -40px (no room to zoom out).
                  Pass the ending camera state so SceneComplete eases back
                  to neutral over 40 frames rather than jumping. */}
              <SceneComplete initialScale={1.15} initialOffsetY={-40} />
            </div>
          </div>
        </Sequence>
        {/* Seq 4 – CTA: absolute overlay at canvas size */}
        <Sequence from={840} durationInFrames={240}>
          <div style={{ position: "absolute", left: 0, top: 0, width, height }}>
            <SceneCta
              totalDuration={240}
              layout="portrait"
              portraitTermTop={PORTRAIT_TERM_TOP}
              portraitTermLeft={PORTRAIT_TERM_LEFT}
              portraitScale={PORTRAIT_SCENE_SCALE}
            />
          </div>
        </Sequence>
        <FilmGrain width={1080} height={1920} opacity={0.035} />
        <Vignette intensity={0.55} />
      </div>
    );
  }

  // ── Square (1:1) and Wide (16:9) layouts ───────────────────────────────────
  const termShift = isSquare ? SQUARE_TERM_OFFSET : 0;
  const shiftStyle = isSquare
    ? { transform: `translateX(${SQUARE_TERM_OFFSET}px)` }
    : {};
  return (
    <div style={{ position: "relative", width: 1920, height: 1080 }}>
      <Sequence from={0} durationInFrames={60}>
        <div style={{ width: "100%", height: "100%", ...shiftStyle }}>
          <InstantHook />
        </div>
      </Sequence>
      <Sequence from={60} durationInFrames={660}>
        <div style={{ width: "100%", height: "100%", ...shiftStyle }}>
          <FastPipeline />
        </div>
      </Sequence>
      <Sequence from={720} durationInFrames={120}>
        <div style={{ width: "100%", height: "100%", ...shiftStyle }}>
          {/* FastPipeline ends at 1.15x (no room to zoom out — confirm widget
              is present all the way to the last compressed frame). Start
              SceneComplete zoomed in and let it pull back over 40 frames. */}
          <SceneComplete initialScale={1.15} initialOffsetY={-40} />
        </div>
      </Sequence>
      {/* CTA: absolute overlay sized to the actual canvas so square layout centres correctly */}
      <Sequence from={840} durationInFrames={240}>
        <div style={{ position: "absolute", left: 0, top: 0, width, height }}>
          <SceneCta
            totalDuration={240}
            layout={isSquare ? "square" : "wide"}
            terminalOffsetX={termShift}
          />
        </div>
      </Sequence>
      <FilmGrain width={1920} height={1080} opacity={0.035} />
      <Vignette intensity={0.55} />
    </div>
  );
};
