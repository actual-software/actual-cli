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

// Portrait (9:16) constants.
// Scale: 1200 × 0.9 = 1080px → fills the canvas width exactly.
// TermTop: vertical offset from canvas top where the scaled terminal sits.
// This leaves ~1000px below the terminal for the CTA text block.
const PORTRAIT_SCALE = 0.9;
const PORTRAIT_TERM_TOP = 200;

// Wraps a scene component (InstantHook / FastPipeline / SceneComplete) for the
// portrait canvas. The scene renders into a 1200×640 logical area (matching the
// TerminalWindow dimensions) which is then CSS-scaled so it fills the 1080px
// canvas width, anchored at the top-left corner.
const PortraitSceneWrapper: React.FC<{
  children: React.ReactNode;
  top: number;
  scale: number;
}> = ({ children, top, scale }) => (
  <div
    style={{
      position: "absolute",
      left: 0,
      top: 0,
      width: 1080,
      height: 1920,
      background: COLORS.background,
      overflow: "hidden",
    }}
  >
    {/* 1200×640 logical area scaled to fit portrait canvas width.
        transformOrigin: top-left so scale(0.9) → visual 1080×576,
        anchored cleanly at (0, top). */}
    <div
      style={{
        position: "absolute",
        left: 0,
        top,
        width: 1200,
        height: 640,
        transform: `scale(${scale})`,
        transformOrigin: "top left",
      }}
    >
      {children}
    </div>
  </div>
);

// Short clip: 1080 frames (18s at 60fps)
// Hook: 60f, FastPipeline: 660f (full pipeline compressed), Complete: 120f, CTA: 240f
//
// Three layout modes driven by canvas dimensions:
//   Wide (16:9, 1920×1080)  — outer container is 1920×1080, terminal centred.
//   Square (1:1, 1080×1080) — same 1920×1080 container, terminal shifted left
//                             by SQUARE_TERM_OFFSET to expose the logo+steps panel.
//   Portrait (9:16, 1080×1920) — native 1080×1920 container; terminal scaled to
//                             fill canvas width, positioned near top to leave room
//                             for the CTA text below.
const SQUARE_TERM_OFFSET = -162; // 15% of 1080px

export const ShortClip: React.FC = () => {
  const { width, height } = useVideoConfig();
  const isSquare = width === height;
  const isPortrait = height > width; // 9:16 (1080×1920)

  // ── Portrait (9:16) layout ─────────────────────────────────────────────────
  if (isPortrait) {
    return (
      <div style={{ position: "relative", width: 1080, height: 1920 }}>
        <Sequence from={0} durationInFrames={60}>
          <PortraitSceneWrapper top={PORTRAIT_TERM_TOP} scale={PORTRAIT_SCALE}>
            <InstantHook />
          </PortraitSceneWrapper>
        </Sequence>
        <Sequence from={60} durationInFrames={660}>
          <PortraitSceneWrapper top={PORTRAIT_TERM_TOP} scale={PORTRAIT_SCALE}>
            <FastPipeline />
          </PortraitSceneWrapper>
        </Sequence>
        <Sequence from={720} durationInFrames={120}>
          <PortraitSceneWrapper top={PORTRAIT_TERM_TOP} scale={PORTRAIT_SCALE}>
            <SceneComplete />
          </PortraitSceneWrapper>
        </Sequence>
        {/* CTA: absolute overlay at canvas size so portrait layout centres correctly */}
        <Sequence from={840} durationInFrames={240}>
          <div style={{ position: "absolute", left: 0, top: 0, width, height }}>
            <SceneCta
              totalDuration={240}
              layout="portrait"
              portraitTermTop={PORTRAIT_TERM_TOP}
              portraitScale={PORTRAIT_SCALE}
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
          <SceneComplete initialScale={1.15} />
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
